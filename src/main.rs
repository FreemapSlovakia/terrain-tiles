use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use clap::Parser;
use httpdate::HttpDate;
use serde::Deserialize;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};
use terrain_tiles::{
    elevation,
    encode::{self, Format},
    error::AppError,
    mosaic,
    shading::{self, Shading},
    source::Source,
    tile::TileWindow,
};
use tokio::sync::Semaphore;

#[derive(Parser)]
#[command(about)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:3040")]
    listen: SocketAddr,

    /// A source as KEY=PATH; repeat it, highest priority first. KEY is what
    /// `X-Attribution` names as `s<KEY>`.
    #[arg(long = "source", required = true, value_parser = Source::parse_arg)]
    sources: Vec<(String, PathBuf)>,

    /// Highest zoom served; default the finest source's. Above it the client
    /// should overzoom: shading upsampled heights only sharpens noise.
    #[arg(long)]
    max_zoom: Option<u8>,

    /// GDAL block cache, MiB.
    #[arg(long, default_value_t = 1024)]
    cache_mb: usize,

    /// Tiles worked on at once; default one per CPU.
    #[arg(long)]
    workers: Option<usize>,

    /// Open handles per source, shared by the workers. Each holds the part of
    /// the file's tile index it has read, up to hundreds of MB at z18.
    #[arg(long, default_value_t = 2)]
    handles: usize,

    /// Seconds a pooled handle may stay idle before it is closed.
    #[arg(long, default_value_t = 60)]
    evict_after: u64,

    /// Seconds a client may reuse a tile without asking; 0 revalidates every
    /// time, which a 304 answers without reading or rendering anything.
    #[arg(long, default_value_t = 0)]
    max_age: u64,
}

const X_ATTRIBUTION: HeaderName = HeaderName::from_static("x-attribution");

struct AppState {
    sources: Vec<Source>,
    max_zoom: u8,
    /// One per worker; taken before a tile's work starts.
    permits: Arc<Semaphore>,
    /// The binary's modification time: a deploy that changes rendering
    /// invalidates every tile.
    code_modified: SystemTime,
    cache_control: HeaderValue,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let workers = args
        .workers
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()));

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(workers)
        .build()?
        .block_on(serve(args, workers))
}

async fn serve(args: Args, workers: usize) -> Result<(), Box<dyn std::error::Error>> {
    gdal::config::set_config_option("GDAL_CACHEMAX", &args.cache_mb.to_string())?;

    let sources = args
        .sources
        .into_iter()
        .map(|(key, path)| Source::open(key, path, args.handles))
        .collect::<Result<Vec<_>, _>>()?;

    for s in &sources {
        eprintln!("source {} at z{}", s.key, s.zoom);
    }

    let max_zoom = args
        .max_zoom
        .unwrap_or_else(|| sources.iter().map(|s| s.zoom).max().unwrap_or(0));

    eprintln!("serving up to z{max_zoom} with {workers} workers");

    let state = Arc::new(AppState {
        sources,
        max_zoom,
        permits: Arc::new(Semaphore::new(workers)),
        code_modified: std::fs::metadata(std::env::current_exe()?)?.modified()?,
        cache_control: HeaderValue::from_str(&if args.max_age == 0 {
            "public, no-cache".to_string()
        } else {
            format!("public, max-age={}", args.max_age)
        })?,
    });

    let evict_after = Duration::from_secs(args.evict_after);

    std::thread::spawn({
        let state = state.clone();

        move || maintain(&state, evict_after)
    });

    let app = Router::new()
        .route("/elevation/{z}/{x}/{y}", get(elevation))
        .route("/hillshade/{z}/{x}/{y}", get(hillshade))
        .layer(middleware::map_response(cors))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(args.listen).await?;

    eprintln!("listening on {}", args.listen);

    axum::serve(listener, app).await?;

    Ok(())
}

/// Closes idle handles every few seconds and logs the pools' counters each
/// minute, for sources that saw any use.
fn maintain(state: &AppState, evict_after: Duration) {
    const TICK: Duration = Duration::from_secs(10);
    const LOG_EVERY: u32 = 6;

    for tick in 1u32.. {
        std::thread::sleep(TICK);

        for s in &state.sources {
            s.evict(evict_after);
        }

        if tick % LOG_EVERY != 0 {
            continue;
        }

        let mut line = String::new();

        for s in &state.sources {
            let (st, skipped) = s.take_stats();

            if st.checkouts == 0 && skipped == 0 && st.evicted == 0 {
                continue;
            }

            line.push_str(&format!(
                " {}[checkouts={} skipped={skipped} waits={} wait_total={:.1}s wait_max={:.2}s peak={} open={} opened={} evicted={}]",
                s.key,
                st.checkouts,
                st.waits,
                st.wait_total.as_secs_f64(),
                st.wait_max.as_secs_f64(),
                st.in_use_peak,
                st.open,
                st.opened,
                st.evicted,
            ));
        }

        if !line.is_empty() {
            eprintln!("pools:{line}");
        }
    }
}

/// On every answer, errors too: otherwise the browser hides a 404 for a tile
/// outside coverage behind a CORS error.
async fn cors(mut response: Response) -> Response {
    let h = response.headers_mut();

    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("x-attribution"),
    );

    response
}

/// When the tile last changed: the newest of the sources that may cover it and
/// the binary. Decided from the footprints, so it costs no read.
fn tile_modified(state: &AppState, w: &TileWindow) -> SystemTime {
    state
        .sources
        .iter()
        .filter(|s| s.covers(w))
        .map(|s| s.modified)
        .fold(state.code_modified, SystemTime::max)
}

/// Whether the client's copy, dated by `If-Modified-Since`, is still current.
fn is_fresh(request: &HeaderMap, modified: SystemTime) -> bool {
    request
        .get(header::IF_MODIFIED_SINCE)
        .and_then(|v| v.to_str().ok()?.parse::<HttpDate>().ok())
        .is_some_and(|since| HttpDate::from(modified) <= since)
}

struct Tile {
    body: Vec<u8>,
    content_type: &'static str,
    /// Source keys for `X-Attribution`.
    credited: Vec<String>,
}

/// Answers 304 when the client's copy is current, else runs `work` once a
/// worker is free. A request the client drops while waiting for a worker is
/// never worked on, which is most of them when zooming fast.
async fn serve_tile(
    state: Arc<AppState>,
    (z, x, y): (u8, u32, u32),
    request: &HeaderMap,
    work: impl FnOnce(&[Source], TileWindow) -> Result<Tile, AppError> + Send + 'static,
) -> Result<Response, AppError> {
    let w = TileWindow::new(z, x, y, state.max_zoom).ok_or(AppError::NotFound)?;
    let modified = tile_modified(&state, &w);

    let mut headers = HeaderMap::new();

    headers.insert(header::CACHE_CONTROL, state.cache_control.clone());

    if let Ok(v) = HeaderValue::from_str(&httpdate::fmt_http_date(modified)) {
        headers.insert(header::LAST_MODIFIED, v);
    }

    if is_fresh(request, modified) {
        return Ok((StatusCode::NOT_MODIFIED, headers).into_response());
    }

    let permit = state
        .permits
        .clone()
        .acquire_owned()
        .await
        .expect("semaphore closed");

    let tile = tokio::task::spawn_blocking(move || {
        let _permit = permit;

        work(&state.sources, w)
    })
    .await
    .expect("tile task panicked")?;

    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(tile.content_type),
    );

    let attribution = tile
        .credited
        .iter()
        .map(|k| format!("s{k}"))
        .collect::<Vec<_>>()
        .join(",");

    if let Ok(v) = HeaderValue::from_str(&attribution) {
        headers.insert(X_ATTRIBUTION, v);
    }

    Ok((headers, tile.body).into_response())
}

async fn elevation(
    State(state): State<Arc<AppState>>,
    Path(zxy): Path<(u8, u32, u32)>,
    request: HeaderMap,
) -> Result<Response, AppError> {
    serve_tile(state, zxy, &request, |sources, w| {
        let m = mosaic::read(sources, &w)?.ok_or(AppError::NotFound)?;

        Ok(Tile {
            body: elevation::encode(&m.heights, w.z)?,
            content_type: "application/octet-stream",
            credited: m.credited,
        })
    })
    .await
}

#[derive(Deserialize)]
struct HillshadeQuery {
    shading: Option<String>,
    #[serde(default)]
    format: Format,
}

async fn hillshade(
    State(state): State<Arc<AppState>>,
    Path(zxy): Path<(u8, u32, u32)>,
    Query(q): Query<HillshadeQuery>,
    request: HeaderMap,
) -> Result<Response, AppError> {
    let shading = Shading::parse(q.shading.as_deref().unwrap_or(shading::DEFAULT))?;

    serve_tile(state, zxy, &request, move |sources, w| {
        // Outside all coverage the tile is the background, not a 404, so a
        // client can keep its error tile for real failures.
        let (planes, credited) = match mosaic::read(sources, &w)? {
            Some(m) => (shading.render(&m.heights, &w), m.credited),
            None => (shading.background_planes(), Vec::new()),
        };

        let (body, content_type) = encode::encode(&planes, q.format, shading.is_gray())?;

        Ok(Tile {
            body,
            content_type,
            credited,
        })
    })
    .await
}

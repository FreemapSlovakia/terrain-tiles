use crate::{
    error::AppError,
    footprint::Footprint,
    pool::{Pool, Stats},
    tile::{PX_Z0, SIZE, TileWindow},
};
use gdal::{Dataset, raster::ResampleAlg};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime},
};

/// One per-country DEM: EPSG:3857, on the XYZ pixel grid of `zoom`.
pub struct Source {
    pub key: String,
    /// The file's modification time; a tile it covers is no older.
    pub modified: SystemTime,
    pool: Pool,
    footprint: Option<Footprint>,
    /// Tiles in the file's extent that the footprint turned away.
    skipped: AtomicU64,
    west: f64,
    north: f64,
    width: usize,
    height: usize,
    px: f64,
    pub zoom: u8,
    scale: f64,
    offset: f64,
    nodata: Option<f64>,
}

impl Source {
    /// Parses a `KEY=PATH` argument.
    pub fn parse_arg(s: &str) -> Result<(String, PathBuf), String> {
        let (key, path) = s.split_once('=').ok_or("expected KEY=PATH")?;

        Ok((key.to_string(), PathBuf::from(path)))
    }

    pub fn open(key: String, path: PathBuf, max_open: usize) -> Result<Self, AppError> {
        let ds = Dataset::open(&path)?;

        let epsg = ds.spatial_ref()?.auth_code()?;

        if epsg != 3857 {
            return Err(AppError::Config(format!(
                "{}: EPSG:{epsg}, not EPSG:3857",
                path.display()
            )));
        }

        let gt = ds.geo_transform()?;

        if gt[2] != 0.0 || gt[4] != 0.0 || (gt[1] + gt[5]).abs() > 1e-9 * gt[1] {
            return Err(AppError::Config(format!(
                "{}: pixels are not square and north-up",
                path.display()
            )));
        }

        let zoom_f = (PX_Z0 / gt[1]).log2();
        let zoom = zoom_f.round();

        if (zoom_f - zoom).abs() > 1e-6 {
            return Err(AppError::Config(format!(
                "{}: pixel size {} is not an XYZ zoom level",
                path.display(),
                gt[1]
            )));
        }

        let (width, height) = ds.raster_size();
        let band = ds.rasterband(1)?;
        let (scale, offset, nodata) = (
            band.scale().unwrap_or(1.0),
            band.offset().unwrap_or(0.0),
            band.no_data_value(),
        );

        let footprint = Footprint::build(&ds)?;
        let modified = std::fs::metadata(&path)?.modified()?;

        Ok(Self {
            key,
            modified,
            // The handle opened here to measure the file is the pool's first.
            pool: Pool::new(&path, ds, max_open),
            footprint,
            skipped: AtomicU64::new(0),
            west: gt[0],
            north: gt[3],
            width,
            height,
            px: gt[1],
            zoom: zoom as u8,
            scale,
            offset,
            nodata,
        })
    }

    /// Whether the source may have data under the window: its extent, then
    /// its footprint.
    pub fn intersects(&self, w: &TileWindow) -> bool {
        let bounds = w.bounds();

        if !self.in_extent(bounds) {
            return false;
        }

        let covered = self.footprint_covers(bounds);

        if !covered {
            self.skipped.fetch_add(1, Ordering::Relaxed);
        }

        covered
    }

    /// As `intersects`, without counting a skip.
    pub fn covers(&self, w: &TileWindow) -> bool {
        let bounds = w.bounds();

        self.in_extent(bounds) && self.footprint_covers(bounds)
    }

    fn footprint_covers(&self, bounds: (f64, f64, f64, f64)) -> bool {
        self.footprint.as_ref().is_none_or(|f| f.covers(bounds))
    }

    fn in_extent(&self, (west, south, east, north): (f64, f64, f64, f64)) -> bool {
        west < self.west + self.width as f64 * self.px
            && east > self.west
            && south < self.north
            && north > self.north - self.height as f64 * self.px
    }

    /// Closes pooled handles idle for longer than `after`.
    pub fn evict(&self, after: Duration) {
        self.pool.evict(after);
    }

    /// Pool counters and footprint skips since the previous call.
    pub fn take_stats(&self) -> (Stats, u64) {
        (
            self.pool.take_stats(),
            self.skipped.swap(0, Ordering::Relaxed),
        )
    }

    /// Heights for the buffered window, NaN where this source has no data.
    pub fn read(&self, w: &TileWindow) -> Result<Vec<f32>, AppError> {
        // Window origin in this source's pixels; `f` source pixels per output pixel.
        let f = w.px() / self.px;
        let (west, _, _, north) = w.bounds();
        let c0 = (west - self.west) / self.px;
        let r0 = (self.north - north) / self.px;

        if f >= 1.0 {
            self.read_downsampled(c0.round() as isize, r0.round() as isize, f.round() as usize)
        } else {
            self.read_upsampled(c0, r0, f)
        }
    }

    /// Runs `op` on a pooled handle's band.
    fn with_band<T>(
        &self,
        op: impl FnOnce(&gdal::raster::RasterBand) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        self.pool.with(|ds| op(&ds.rasterband(1)?))
    }

    fn value(&self, raw: i32) -> f32 {
        if self.nodata == Some(raw as f64) {
            f32::NAN
        } else {
            (raw as f64 * self.scale + self.offset) as f32
        }
    }

    /// At or below the native zoom: whole output pixels map onto `f` source
    /// pixels, so GDAL reads the matching overview.
    fn read_downsampled(&self, c0: isize, r0: isize, f: usize) -> Result<Vec<f32>, AppError> {
        let mut out = vec![f32::NAN; SIZE * SIZE];
        let fi = f as isize;

        let clip = |start: isize, len: usize| {
            let lo = if start < 0 { (-start + fi - 1) / fi } else { 0 };
            let hi = ((len as isize - start) / fi).min(SIZE as isize);

            (lo.max(0) as usize, hi.max(0) as usize)
        };

        let (ox0, ox1) = clip(c0, self.width);
        let (oy0, oy1) = clip(r0, self.height);

        if ox0 >= ox1 || oy0 >= oy1 {
            return Ok(out);
        }

        let (ow, oh) = (ox1 - ox0, oy1 - oy0);

        let buf = self.with_band(|band| {
            Ok(band.read_as::<i32>(
                (c0 + ox0 as isize * fi, r0 + oy0 as isize * fi),
                (ow * f, oh * f),
                (ow, oh),
                Some(ResampleAlg::Average),
            )?)
        })?;

        let data = buf.data();

        for y in 0..oh {
            for x in 0..ow {
                out[(oy0 + y) * SIZE + ox0 + x] = self.value(data[y * ow + x]);
            }
        }

        Ok(out)
    }

    /// Above the native zoom: bicubic between source pixel centres.
    fn read_upsampled(&self, c0: f64, r0: f64, f: f64) -> Result<Vec<f32>, AppError> {
        let mut out = vec![f32::NAN; SIZE * SIZE];

        // Source-pixel-centre coordinates of the first and last output pixel centre.
        let first = |o: f64| o + 0.5 * f - 0.5;
        let last = |o: f64| o + (SIZE as f64 - 0.5) * f - 0.5;

        let sx0 = (first(c0).floor() as isize - 1).max(0);
        let sy0 = (first(r0).floor() as isize - 1).max(0);
        let sx1 = (last(c0).ceil() as isize + 2).min(self.width as isize);
        let sy1 = (last(r0).ceil() as isize + 2).min(self.height as isize);

        if sx0 >= sx1 || sy0 >= sy1 {
            return Ok(out);
        }

        let (sw, sh) = ((sx1 - sx0) as usize, (sy1 - sy0) as usize);

        let buf = self
            .with_band(|band| Ok(band.read_as::<i32>((sx0, sy0), (sw, sh), (sw, sh), None)?))?;

        let src: Vec<f32> = buf.data().iter().map(|&v| self.value(v)).collect();

        // A row's or column's taps and weights are the same for every pixel on it.
        let taps = |o: f64, s0: isize, len: usize| -> Vec<Option<(usize, [f64; 4])>> {
            (0..SIZE)
                .map(|i| {
                    let t = first(o) + i as f64 * f - s0 as f64;
                    let i0 = t.floor() as isize;

                    (i0 >= 1 && i0 + 2 < len as isize)
                        .then(|| ((i0 - 1) as usize, cubic_weights(t - i0 as f64)))
                })
                .collect()
        };

        let cols = taps(c0, sx0, sw);

        for (y, row) in taps(r0, sy0, sh).iter().enumerate() {
            let Some((iy, wy)) = row else { continue };

            for (x, col) in cols.iter().enumerate() {
                if let Some((ix, wx)) = col {
                    out[y * SIZE + x] = bicubic(&src[iy * sw + ix..], sw, wx, wy);
                }
            }
        }

        Ok(out)
    }
}

/// Catmull-Rom, as GDAL's cubic.
pub fn cubic_weights(t: f64) -> [f64; 4] {
    let t2 = t * t;
    let t3 = t2 * t;

    [
        -0.5 * t3 + t2 - 0.5 * t,
        1.5 * t3 - 2.5 * t2 + 1.0,
        -1.5 * t3 + 2.0 * t2 + 0.5 * t,
        0.5 * t3 - 0.5 * t2,
    ]
}

/// The 4×4 taps from the top-left of `src`, rows `stride` apart; NaN if any tap is.
pub fn bicubic(src: &[f32], stride: usize, wx: &[f64; 4], wy: &[f64; 4]) -> f32 {
    let mut sum = 0.0;

    for (j, wyj) in wy.iter().enumerate() {
        for (i, wxi) in wx.iter().enumerate() {
            sum += src[j * stride + i] as f64 * wyj * wxi;
        }
    }

    sum as f32
}

//! Shading above a source's native zoom. Shading heights upsampled from its
//! grid would give every cell a curved patch of its own, a grid of pillows;
//! such pixels are shaded where the data is, on the ancestor tile at the
//! source's zoom, and that picture is scaled up.

use crate::{
    error::AppError,
    mosaic,
    shading::{self, Planes, Shading},
    source::{Source, bicubic, cubic_weights},
    tile::{BUFFER, SIZE, TILE, TileWindow},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

/// An ancestor tile's shading, premultiplied RGBA. 16 bits, as it is
/// interpolated and quantized again for the child's encoding.
type Picture = Arc<Vec<[u16; 4]>>;

/// Canonical shading, zoom, x, y.
type Key = (Arc<str>, u8, u32, u32);

/// Filled by the first worker to need the ancestor; the others wait on it.
/// Left empty if that render fails or panics, for the next to retry.
type Slot = Arc<Mutex<Option<Picture>>>;

/// Rendered ancestor tiles, each serving up to 4^k children, least recently
/// used dropped first. Other shadings are evicted first while they hold over
/// half the cap, so one-off shadings, as a client dragging a shading slider
/// sends, cannot flush the default's ancestors that every other visitor needs.
pub struct Ancestors {
    cap: usize,
    /// `Key`'s shading for `shading::DEFAULT`, and for it on the opaque white
    /// background a client asks for when the shading is its base layer.
    default: [Arc<str>; 2],
    cache: Mutex<Cache>,
}

struct Entry {
    slot: Slot,
    used: u64,
    default: bool,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<Key, Entry>,
    clock: u64,
    /// Entries of shadings other than the default.
    others: usize,
}

impl Cache {
    fn lru(&self, default: bool) -> Option<Key> {
        self.entries
            .iter()
            .filter(|(_, e)| e.default == default)
            .min_by_key(|(_, e)| e.used)
            .map(|(k, _)| k.clone())
    }

    /// The slot for `key`, added if new.
    fn slot(&mut self, key: Key, default: bool, cap: usize) -> Slot {
        self.clock += 1;

        let used = self.clock;

        if let Some(e) = self.entries.get_mut(&key) {
            e.used = used;

            return e.slot.clone();
        }

        let slot = Slot::default();

        self.entries.insert(
            key,
            Entry {
                slot: slot.clone(),
                used,
                default,
            },
        );

        if !default {
            self.others += 1;
        }

        if self.entries.len() > cap {
            let evict_default = self.others <= cap / 2;

            // Whichever kind is over its share has an entry to evict.
            if let Some(k) = self.lru(evict_default)
                && self.entries.remove(&k).is_some_and(|e| !e.default)
            {
                self.others -= 1;
            }
        }

        slot
    }
}

impl Ancestors {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            default: [
                shading::DEFAULT.to_string(),
                shading::DEFAULT.replacen("00000000", "ffffffff", 1),
            ]
            .map(|s| shading_key(&Shading::parse(&s).expect("default shading"))),
            cache: Mutex::default(),
        }
    }

    fn get(
        &self,
        sources: &[Source],
        (key, shading): (&Arc<str>, &Shading),
        (z, x, y): (u8, u32, u32),
    ) -> Result<Picture, AppError> {
        let slot = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slot((key.clone(), z, x, y), self.default.contains(key), self.cap);

        let mut filled = slot.lock().unwrap_or_else(PoisonError::into_inner);

        if let Some(p) = &*filled {
            return Ok(p.clone());
        }

        let w = TileWindow::new(z, x, y, z).ok_or(AppError::NotFound)?;

        let planes = match mosaic::read(sources, &w, false)? {
            Some(m) => shading.render(&m.heights, &w),
            None => shading.background_planes(),
        };

        let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;

        let picture: Picture = Arc::new(
            (0..TILE * TILE)
                .map(|i| {
                    [
                        q(planes.r[i]),
                        q(planes.g[i]),
                        q(planes.b[i]),
                        q(planes.a[i]),
                    ]
                })
                .collect(),
        );

        *filled = Some(picture.clone());

        Ok(picture)
    }
}

/// The cache key of `shading`: spellings of the same shading share ancestors.
pub fn shading_key(shading: &Shading) -> Arc<str> {
    format!("{shading:?}").into()
}

/// Ancestor-zoom pixels `x0..x1`, `y0..y1` holding every bicubic tap of the
/// tile at `w` scaled up from zoom `za`.
fn tap_box(w: &TileWindow, za: u8) -> (i64, i64, i64, i64) {
    let s = (1u64 << (w.z - za)) as f64;
    let at = |o: u32, p: f64| (o as f64 * TILE as f64 + p + 0.5) / s - 0.5;
    let first = |o: u32| at(o, 0.0).floor() as i64 - 1;
    let end = |o: u32| at(o, (TILE - 1) as f64).floor() as i64 + 3;

    (first(w.x), first(w.y), end(w.x), end(w.y))
}

/// The ancestor tiles at zoom `za` under `tap_box`, wrapped at the
/// antimeridian and clamped at the poles.
fn ancestor_tiles(w: &TileWindow, za: u8) -> Vec<(u32, u32)> {
    let (x0, y0, x1, y1) = tap_box(w, za);
    let n = 1i64 << za;
    let t = TILE as i64;
    let mut tiles = Vec::new();

    for ty in (y0.div_euclid(t)..=(y1 - 1).div_euclid(t)).map(|ty| ty.clamp(0, n - 1)) {
        for tx in (x0.div_euclid(t)..=(x1 - 1).div_euclid(t)).map(|tx| tx.rem_euclid(n)) {
            let tile = (tx as u32, ty as u32);

            if !tiles.contains(&tile) {
                tiles.push(tile);
            }
        }
    }

    tiles
}

/// The source zooms coarser than `z`, ascending.
fn coarse_zooms(sources: &[Source], z: u8) -> Vec<u8> {
    let mut zooms: Vec<u8> = sources
        .iter()
        .map(|s| s.zoom)
        .filter(|&za| za < z)
        .collect();

    zooms.sort_unstable();
    zooms.dedup();

    zooms
}

/// The ancestor windows a hillshade tile at `w` may be scaled up from: one set
/// per source zoom coarser than `w.z`.
pub fn dependencies(w: &TileWindow, sources: &[Source]) -> Vec<TileWindow> {
    coarse_zooms(sources, w.z)
        .into_iter()
        .flat_map(|za| {
            ancestor_tiles(w, za)
                .into_iter()
                .filter_map(move |(x, y)| TileWindow::new(za, x, y, za))
        })
        .collect()
}

/// In `Plan::zooms`, a pixel shaded on the tile itself.
const DIRECT: u8 = u8::MAX;

/// Per ancestor zoom, its tiles by x, y.
type Pictures = Vec<(u8, HashMap<(u32, u32), Picture>)>;

/// Where each pixel of a tile gets its shading.
pub struct Plan {
    /// Per tile pixel, the zoom of the ancestor it is scaled up from, or `DIRECT`.
    zooms: Vec<u8>,
    pictures: Pictures,
}

impl Plan {
    /// Plans the tile at `w` from `native`, per buffered window pixel the zoom
    /// of the source that filled it. Pixels without data take the finest
    /// coarser zoom, whose ancestor has shading where upsampling left a
    /// source's edge without heights. Pixels whose ancestors fail are shaded
    /// directly, pillowed, rather than failing the tile.
    pub fn new(
        native: &[u8],
        w: &TileWindow,
        sources: &[Source],
        shading: (&Arc<str>, &Shading),
        ancestors: &Ancestors,
    ) -> Self {
        let coarse = coarse_zooms(sources, w.z);
        let fill = coarse.last().copied().unwrap_or(DIRECT);

        let mut zooms: Vec<u8> = (0..TILE * TILE)
            .map(
                |i| match native[(i / TILE + BUFFER) * SIZE + i % TILE + BUFFER] {
                    u8::MAX => fill,
                    n if n >= w.z => DIRECT,
                    n => n,
                },
            )
            .collect();

        let mut used = [false; 256];

        for &z in &zooms {
            used[z as usize] = true;
        }

        let mut pictures = Vec::new();

        for za in coarse.into_iter().filter(|&za| used[za as usize]) {
            let got = ancestor_tiles(w, za)
                .into_iter()
                .map(|t| Ok((t, ancestors.get(sources, shading, (za, t.0, t.1))?)))
                .collect::<Result<HashMap<_, _>, AppError>>();

            match got {
                Ok(p) => pictures.push((za, p)),
                Err(e) => {
                    eprintln!("hillshade {}/{}/{}: ancestors at z{za}: {e}", w.z, w.x, w.y);

                    for z in zooms.iter_mut().filter(|z| **z == za) {
                        *z = DIRECT;
                    }
                }
            }
        }

        Self { zooms, pictures }
    }

    /// Whether any pixel is shaded on the tile itself.
    pub fn any_direct(&self) -> bool {
        self.zooms.contains(&DIRECT)
    }

    /// Overwrites the scaled-up pixels of the tile at `w`, scaled up bicubically.
    pub fn paint(&self, planes: &mut Planes, w: &TileWindow) {
        for (za, pictures) in &self.pictures {
            let za = *za;

            // The taps, copied out of the ancestors once, one plane per channel.
            let (x0, y0, x1, y1) = tap_box(w, za);
            let (rw, rh) = ((x1 - x0) as usize, (y1 - y0) as usize);
            let side = (TILE as i64) << za;
            let mut region = vec![vec![0.0f32; rw * rh]; 4];

            for ry in 0..rh {
                let gy = (y0 + ry as i64).clamp(0, side - 1) as usize;

                for rx in 0..rw {
                    let gx = (x0 + rx as i64).rem_euclid(side) as usize;
                    let p = &pictures[&((gx / TILE) as u32, (gy / TILE) as u32)];
                    let v = p[(gy % TILE) * TILE + gx % TILE];

                    for (plane, c) in region.iter_mut().zip(v) {
                        plane[ry * rw + rx] = c as f32 / 65535.0;
                    }
                }
            }

            // A row's or column's taps and weights are the same for every pixel on it.
            let s = (1u64 << (w.z - za)) as f64;

            let taps = |o: u32, r0: i64| -> Vec<(usize, [f64; 4])> {
                (0..TILE)
                    .map(|i| {
                        let t = (o as f64 * TILE as f64 + i as f64 + 0.5) / s - 0.5;
                        let t0 = t.floor();

                        ((t0 as i64 - 1 - r0) as usize, cubic_weights(t - t0))
                    })
                    .collect()
            };

            let cols = taps(w.x, x0);
            let rows = taps(w.y, y0);

            for (i, &n) in self.zooms.iter().enumerate() {
                if n != za {
                    continue;
                }

                let ((ix, wx), (iy, wy)) = (&cols[i % TILE], &rows[i / TILE]);
                let [r, g, b, a] =
                    [0, 1, 2, 3].map(|c| bicubic(&region[c][iy * rw + ix..], rw, wx, wy));

                // Catmull-Rom overshoots; keep the colour premultiplied.
                let a = a.clamp(0.0, 1.0);

                planes.a[i] = a;
                planes.r[i] = r.clamp(0.0, a);
                planes.g[i] = g.clamp(0.0, a);
                planes.b[i] = b.clamp(0.0, a);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str, x: u32) -> Key {
        (Arc::from(s), 12, x, 0)
    }

    #[test]
    fn other_shadings_cannot_flush_the_default() {
        let (mut cache, cap) = (Cache::default(), 10);

        for x in 0..8 {
            cache.slot(key("default", x), true, cap);
        }

        for n in 0..100 {
            cache.slot(key(&format!("slider{n}"), 0), false, cap);
        }

        assert_eq!(cache.entries.len(), cap);
        assert_eq!(cache.others, cap / 2);
        assert!((3..8).all(|x| cache.entries.contains_key(&key("default", x))));
    }

    #[test]
    fn either_kind_fills_the_cap_alone() {
        for default in [true, false] {
            let (mut cache, cap) = (Cache::default(), 10);

            for x in 0..30 {
                cache.slot(key("s", x), default, cap);
            }

            assert_eq!(cache.entries.len(), cap);
            assert!((20..30).all(|x| cache.entries.contains_key(&key("s", x))));
        }
    }
}

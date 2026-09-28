//! Port of the web client's `shading.wgsl`, reading the same `shading=` string
//! the client writes to its URL (`serializeShading`). Each method runs as a flat
//! pass over whole planes, chosen outside the pixel loop, so the loops vectorize.

use crate::{
    error::AppError,
    tile::{BUFFER, SIZE, TILE, TileWindow},
};
use std::f32::consts::{FRAC_PI_2, PI, TAU};

type Rgba = [f32; 4];

const N: usize = TILE * TILE;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Method {
    HillshadeIgor,
    HillshadeClassic,
    SlopeIgor,
    SlopeClassic,
    ColorRelief,
    Aspect,
}

#[derive(Debug)]
struct Component {
    method: Method,
    contrast: f32,
    brightness: f32,
    exaggeration: f32,
    stops: Vec<(f32, Rgba)>,
    // Derived once from azimuth and altitude rather than per pixel.
    light: [f32; 3],
    zenith_sin: f32,
    aspect_ref: f32,
}

#[derive(Debug)]
pub struct Shading {
    background: Rgba,
    components: Vec<Component>,
}

/// Premultiplied colour planes, `TILE`² each.
pub struct Planes {
    pub r: Vec<f32>,
    pub g: Vec<f32>,
    pub b: Vec<f32>,
    pub a: Vec<f32>,
}

/// The client's default, as `gdaldem hillshade` draws: classic hillshade from
/// 315°, 45° high, white over opaque black.
pub const DEFAULT: &str = "000000ff!hillshade-classic_315.0_45.0_1.0_ffffffff";

fn color(s: &str) -> Result<Rgba, AppError> {
    let bad = || AppError::BadRequest(format!("bad color {s}"));

    if !(s.len() == 6 || s.len() == 8) || !s.is_ascii() {
        return Err(bad());
    }

    let byte = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| bad());

    Ok([
        byte(0)? as f32 / 255.0,
        byte(2)? as f32 / 255.0,
        byte(4)? as f32 / 255.0,
        if s.len() == 8 {
            byte(6)? as f32 / 255.0
        } else {
            1.0
        },
    ])
}

const MAX_COMPONENTS: usize = 8;

impl Shading {
    pub fn parse(s: &str) -> Result<Self, AppError> {
        let mut parts = s.split('!');
        let background = color(parts.next().unwrap_or_default())?;
        let mut components = Vec::new();

        for (n, part) in parts.enumerate() {
            // Each component is a full pass over the tile, and every string renders anew.
            if n == MAX_COMPONENTS {
                return Err(AppError::BadRequest(format!(
                    "more than {MAX_COMPONENTS} components"
                )));
            }

            let mut params: Vec<&str> = part.split('_').collect();

            // `type~contrast~brightness`; the levels are left out when default.
            let mut levels = params.remove(0).split('~');
            let kind = levels.next().unwrap_or_default();

            let num = |s: &str| {
                s.parse::<f32>()
                    .map_err(|_| AppError::BadRequest(format!("bad component {part}")))
            };

            let contrast = levels.next().map(num).transpose()?;
            let brightness = levels.next().map(num).transpose()?;

            let method = match kind {
                "hillshade-igor" => Method::HillshadeIgor,
                "hillshade-classic" => Method::HillshadeClassic,
                "slope-igor" => Method::SlopeIgor,
                "slope-classic" => Method::SlopeClassic,
                "color-relief" => Method::ColorRelief,
                "aspect" => Method::Aspect,
                _ => return Err(AppError::BadRequest(format!("unknown component {kind}"))),
            };

            let (mut azimuth, mut altitude, mut exaggeration) = (f32::NAN, f32::NAN, 1.0);
            let mut stops = Vec::new();

            if matches!(method, Method::ColorRelief | Method::Aspect) {
                for pair in params.chunks(2) {
                    stops.push((
                        num(pair.first().copied().unwrap_or_default())?,
                        color(pair.get(1).unwrap_or(&""))?,
                    ));
                }
            } else {
                stops.push((0.0, color(params.pop().unwrap_or_default())?));

                let mut it = params.into_iter();
                let mut next = || num(it.next().unwrap_or_default());

                if matches!(method, Method::HillshadeIgor | Method::HillshadeClassic) {
                    azimuth = next()?.to_radians();
                }

                if matches!(method, Method::HillshadeClassic | Method::SlopeClassic) {
                    altitude = next()?.to_radians();
                }

                exaggeration = next()?;
            }

            let zenith = FRAC_PI_2 - altitude;
            let zenith_sin = zenith.sin();

            let c = Component {
                method,
                contrast: contrast.unwrap_or(1.0),
                brightness: brightness.unwrap_or(0.0),
                exaggeration,
                stops,
                light: [
                    azimuth.sin() * zenith_sin,
                    -azimuth.cos() * zenith_sin,
                    zenith.cos(),
                ],
                zenith_sin,
                aspect_ref: (azimuth - FRAC_PI_2).rem_euclid(TAU),
            };

            components.push(c);
        }

        Ok(Self {
            background,
            components,
        })
    }

    /// True when every colour involved is a grey, so the output is too.
    pub fn is_gray(&self) -> bool {
        std::iter::once(&self.background)
            .chain(
                self.components
                    .iter()
                    .flat_map(|c| c.stops.iter().map(|s| &s.1)),
            )
            .all(|c| c[0] == c[1] && c[1] == c[2])
    }

    /// A tile with no terrain: the background alone.
    pub fn background_planes(&self) -> Planes {
        let [r, g, b, a] = self.background;

        Planes {
            r: vec![r * a; N],
            g: vec![g * a; N],
            b: vec![b * a; N],
            a: vec![a; N],
        }
    }

    pub fn render(&self, heights: &[f32], w: &TileWindow) -> Planes {
        // Terrain gradient in m/m by Horn's method, NaN where the centre has no
        // data or a neighbour has none; NaN shades as flat ground.
        let mut gx = vec![0.0f32; N];
        let mut gy = vec![0.0f32; N];

        for y in 0..TILE {
            let inv = 1.0 / (8.0 * w.ground_px(y + BUFFER) as f32);
            let r0 = &heights[(y + BUFFER - 1) * SIZE..][..SIZE];
            let r1 = &heights[(y + BUFFER) * SIZE..][..SIZE];
            let r2 = &heights[(y + BUFFER + 1) * SIZE..][..SIZE];
            let (gx, gy) = (&mut gx[y * TILE..][..TILE], &mut gy[y * TILE..][..TILE]);

            for x in 0..TILE {
                let c = x + BUFFER;
                let (nn, zn, pn) = (r0[c - 1], r0[c], r0[c + 1]);
                let (nz, zz, pz) = (r1[c - 1], r1[c], r1[c + 1]);
                let (np, zp, pp) = (r2[c - 1], r2[c], r2[c + 1]);

                // Adding 0·centre makes a missing centre poison the gradient too.
                let hole = zz * 0.0;

                gx[x] = (-nn + pn - 2.0 * nz + 2.0 * pz - np + pp + hole) * inv;
                gy[x] = (-nn - 2.0 * zn - pn + np + 2.0 * zp + pp + hole) * inv;
            }
        }

        let mut acc = Acc {
            sr: vec![0.0; N],
            sg: vec![0.0; N],
            sb: vec![0.0; N],
            sa: vec![0.0; N],
            ap: vec![1.0; N],
        };

        for c in &self.components {
            // The per-pixel colour methods are rarely used, so left scalar.
            let alpha_k = c.contrast * 0.5 + 0.5 + c.brightness;

            match c.method {
                Method::HillshadeClassic => {
                    let l = c.light;

                    acc.add(c, &gx, &gy, |nx, ny, nz| nx * l[0] + ny * l[1] + nz * l[2]);
                }
                Method::SlopeClassic => {
                    let (lz, zs) = (c.light[2], c.zenith_sin);

                    acc.add(c, &gx, &gy, |nx, ny, nz| {
                        lz * nz + zs * (nx * nx + ny * ny).sqrt()
                    });
                }
                Method::SlopeIgor => acc.add(c, &gx, &gy, |_, _, nz| acos01(nz) / FRAC_PI_2),
                Method::HillshadeIgor => {
                    let aref = c.aspect_ref;

                    acc.add(c, &gx, &gy, |nx, ny, nz| {
                        let a = atan2(ny, nx);
                        let a = if a < 0.0 { a + TAU } else { a };
                        let d = (a - aref).abs();
                        let d = if d > PI { TAU - d } else { d };

                        acos01(nz) / FRAC_PI_2 * 2.0 * (1.0 - d / PI)
                    });
                }
                Method::ColorRelief => {
                    for i in 0..N {
                        let h = heights[(i / TILE + BUFFER) * SIZE + i % TILE + BUFFER];
                        let col = interpolate(&c.stops, if h.is_nan() { 0.0 } else { h });

                        acc.put(i, col[3] * alpha_k, col);
                    }
                }
                Method::Aspect => {
                    for i in 0..N {
                        let [nx, ny, nz] = normal(gx[i], gy[i], c.exaggeration);

                        let col = if nz > 0.9999 {
                            [0.0; 4]
                        } else {
                            interpolate(&c.stops, atan2(nx, -ny).rem_euclid(TAU))
                        };

                        acc.put(i, col[3] * alpha_k, col);
                    }
                }
            }
        }

        let Acc { sr, sg, sb, sa, ap } = acc;
        let bg = self.background;
        let mut out = Planes {
            r: vec![0.0; N],
            g: vec![0.0; N],
            b: vec![0.0; N],
            a: vec![0.0; N],
        };

        for i in 0..N {
            let fg_a = 1.0 - ap[i].clamp(0.0, 1.0);
            let inv = if sa[i] == 0.0 { 0.0 } else { 1.0 / sa[i] };
            let under = bg[3] * (1.0 - fg_a);

            out.r[i] = (sr[i] * inv).clamp(0.0, 1.0) * fg_a + bg[0] * under;
            out.g[i] = (sg[i] * inv).clamp(0.0, 1.0) * fg_a + bg[1] * under;
            out.b[i] = (sb[i] * inv).clamp(0.0, 1.0) * fg_a + bg[2] * under;
            out.a[i] = fg_a + under;
        }

        out
    }
}

impl Planes {
    fn to_u8(v: f32) -> u8 {
        (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    }

    /// RGB composited over white.
    pub fn rgb_over_white(&self) -> Vec<u8> {
        (0..N)
            .flat_map(|i| {
                let under = 1.0 - self.a[i];

                [self.r[i], self.g[i], self.b[i]].map(|v| Self::to_u8(v + under))
            })
            .collect()
    }

    /// The red plane composited over white; the image when it is grey.
    pub fn gray_over_white(&self) -> Vec<u8> {
        (0..N)
            .map(|i| Self::to_u8(self.r[i] + 1.0 - self.a[i]))
            .collect()
    }

    /// Straight (not premultiplied) grey and alpha, from the red plane.
    pub fn gray_alpha(&self) -> Vec<u8> {
        (0..N)
            .flat_map(|i| {
                let a = self.a[i].clamp(0.0, 1.0);
                let inv = if a > 0.0 { 1.0 / a } else { 0.0 };

                [Self::to_u8(self.r[i] * inv), Self::to_u8(a)]
            })
            .collect()
    }

    /// Straight (not premultiplied) RGBA.
    pub fn rgba(&self) -> Vec<u8> {
        (0..N)
            .flat_map(|i| {
                let a = self.a[i].clamp(0.0, 1.0);
                let inv = if a > 0.0 { 1.0 / a } else { 0.0 };

                [
                    Self::to_u8(self.r[i] * inv),
                    Self::to_u8(self.g[i] * inv),
                    Self::to_u8(self.b[i] * inv),
                    Self::to_u8(a),
                ]
            })
            .collect()
    }
}

/// Running sums the components blend into, one plane each.
struct Acc {
    sr: Vec<f32>,
    sg: Vec<f32>,
    sb: Vec<f32>,
    sa: Vec<f32>,
    ap: Vec<f32>,
}

impl Acc {
    #[inline(always)]
    fn put(&mut self, i: usize, alpha: f32, col: Rgba) {
        self.sr[i] += alpha * col[0];
        self.sg[i] += alpha * col[1];
        self.sb[i] += alpha * col[2];
        self.sa[i] += alpha;
        self.ap[i] *= 1.0 - alpha;
    }

    /// Blends a single-colour component; generic so each method gets its own
    /// monomorphised, vectorizable loop.
    #[inline(always)]
    fn add(
        &mut self,
        c: &Component,
        gx: &[f32],
        gy: &[f32],
        intensity: impl Fn(f32, f32, f32) -> f32,
    ) {
        let col = c.stops[0].1;
        let e = c.exaggeration;

        for i in 0..N {
            let [nx, ny, nz] = normal(gx[i], gy[i], e);
            let alpha = col[3] * (c.contrast * (intensity(nx, ny, nz) - 0.5) + 0.5 + c.brightness);

            self.put(i, alpha, col);
        }
    }
}

/// Unit surface normal from a gradient; flat where the gradient is NaN.
#[inline(always)]
fn normal(gx: f32, gy: f32, exaggeration: f32) -> [f32; 3] {
    let (x, y) = (-gx * exaggeration, -gy * exaggeration);
    let inv = 1.0 / (x * x + y * y + 1.0).sqrt();

    if inv.is_nan() {
        [0.0, 0.0, 1.0]
    } else {
        [x * inv, y * inv, inv]
    }
}

/// `acos` on [0, 1], error below 7e-5 rad (Abramowitz & Stegun 4.4.45).
#[inline(always)]
fn acos01(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);

    (1.0 - x).sqrt() * (1.570_728_8 + x * (-0.212_114_4 + x * (0.074_261 + x * -0.018_729_3)))
}

/// `atan2`, error about 1e-5 rad.
#[inline(always)]
fn atan2(y: f32, x: f32) -> f32 {
    let (ax, ay) = (x.abs(), y.abs());
    let (hi, lo) = (ax.max(ay), ax.min(ay));
    let a = if hi > 0.0 { lo / hi } else { 0.0 };
    let s = a * a;
    let r = ((-0.046_496_475 * s + 0.159_314_22) * s - 0.327_622_76) * s * a + a;
    let r = if ay > ax { FRAC_PI_2 - r } else { r };
    let r = if x < 0.0 { PI - r } else { r };

    if y < 0.0 { -r } else { r }
}

fn interpolate(stops: &[(f32, Rgba)], t: f32) -> Rgba {
    let Some(first) = stops.first() else {
        return [0.0; 4];
    };

    if stops.len() == 1 || t < first.0 {
        return first.1;
    }

    for pair in stops.windows(2) {
        let ((v0, c0), (v1, c1)) = (pair[0], pair[1]);

        if t < v1 {
            let k = (t - v0) / (v1 - v0);

            return std::array::from_fn(|i| c0[i] + (c1[i] - c0[i]) * k);
        }
    }

    stops[stops.len() - 1].1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The straight per-pixel port of `shading.wgsl`, with exact trigonometry.
    fn reference(s: &Shading, heights: &[f32], w: &TileWindow) -> Vec<Rgba> {
        let at = |x: usize, y: usize| heights[y * SIZE + x];
        let mut out = Vec::with_capacity(N);

        for y in BUFFER..BUFFER + TILE {
            let ground = w.ground_px(y) as f32;

            for x in BUFFER..BUFFER + TILE {
                let e = at(x, y);
                let is_nan = e.is_nan();
                let elev = if is_nan { 0.0 } else { e };

                let normal = |exaggeration: f32| -> [f32; 3] {
                    if is_nan {
                        return [0.0, 0.0, 1.0];
                    }

                    let (nn, nz, np) = (at(x - 1, y - 1), at(x - 1, y), at(x - 1, y + 1));
                    let (zn, zp) = (at(x, y - 1), at(x, y + 1));
                    let (pn, pz, pp) = (at(x + 1, y - 1), at(x + 1, y), at(x + 1, y + 1));
                    let dzdx = -nn + pn - 2.0 * nz + 2.0 * pz - np + pp;
                    let dzdy = -nn - 2.0 * zn - pn + np + 2.0 * zp + pp;
                    let mpp = ground * 8.0 / exaggeration;
                    let (nx, ny) = (-dzdx / mpp, -dzdy / mpp);
                    let len = (nx * nx + ny * ny + 1.0).sqrt();

                    if len.is_nan() {
                        [0.0, 0.0, 1.0]
                    } else {
                        [nx / len, ny / len, 1.0 / len]
                    }
                };

                let (mut sum_rgb, mut sum_alpha, mut alpha_product) = ([0.0f32; 3], 0.0f32, 1.0f32);

                for c in &s.components {
                    let n = if c.method == Method::ColorRelief {
                        [0.0, 0.0, 1.0]
                    } else {
                        normal(c.exaggeration)
                    };

                    let (intensity, color) = match c.method {
                        Method::HillshadeIgor => {
                            let mut diff = (n[1].atan2(n[0]).rem_euclid(TAU) - c.aspect_ref).abs();

                            if diff > PI {
                                diff = TAU - diff;
                            }

                            (
                                n[2].acos() / FRAC_PI_2 * 2.0 * (1.0 - diff / PI),
                                c.stops[0].1,
                            )
                        }
                        Method::HillshadeClassic => (
                            n[0] * c.light[0] + n[1] * c.light[1] + n[2] * c.light[2],
                            c.stops[0].1,
                        ),
                        Method::SlopeIgor => (n[2].acos() / FRAC_PI_2, c.stops[0].1),
                        Method::SlopeClassic => (
                            c.light[2] * n[2] + c.zenith_sin * n[0].hypot(n[1]),
                            c.stops[0].1,
                        ),
                        Method::ColorRelief => (1.0, interpolate(&c.stops, elev)),
                        Method::Aspect => {
                            if n[2] > 0.9999 {
                                (1.0, [0.0; 4])
                            } else {
                                let a = n[0].atan2(-n[1]);

                                (
                                    1.0,
                                    interpolate(&c.stops, if a < 0.0 { a + TAU } else { a }),
                                )
                            }
                        }
                    };

                    let alpha = color[3] * (c.contrast * (intensity - 0.5) + 0.5 + c.brightness);

                    for i in 0..3 {
                        sum_rgb[i] += alpha * color[i];
                    }

                    sum_alpha += alpha;
                    alpha_product *= 1.0 - alpha;
                }

                let fg_a = 1.0 - alpha_product.clamp(0.0, 1.0);
                let bg = s.background;
                let mut px = [0.0; 4];

                for i in 0..3 {
                    let fg = if sum_alpha == 0.0 {
                        0.0
                    } else {
                        (sum_rgb[i] / sum_alpha).clamp(0.0, 1.0)
                    };

                    px[i] = fg * fg_a + bg[i] * bg[3] * (1.0 - fg_a);
                }

                px[3] = fg_a + bg[3] * (1.0 - fg_a);
                out.push(px);
            }
        }

        out
    }

    /// Rough terrain from 0 to ~600 m with holes, cliffs and flat patches.
    fn terrain() -> Vec<f32> {
        let mut h = vec![0.0f32; SIZE * SIZE];
        let mut seed = 12345u32;

        for y in 0..SIZE {
            for x in 0..SIZE {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 8) as f32 / (1 << 24) as f32;
                let (fx, fy) = (x as f32 / 20.0, y as f32 / 27.0);

                h[y * SIZE + x] = if (40..60).contains(&x) && (100..130).contains(&y) {
                    f32::NAN
                } else if x > 200 && y < 50 {
                    300.0
                } else {
                    300.0
                        + 200.0 * fx.sin() * fy.cos()
                        + 30.0 * (fx * 3.1).cos()
                        + noise * 2.0
                        + if x > 150 { 40.0 } else { 0.0 }
                };
            }
        }

        h
    }

    #[test]
    fn matches_the_exact_port_to_within_one_level() {
        let h = terrain();
        let w = TileWindow::new(18, 143_940, 90_180, 20).unwrap();

        for s in [
            "000000ff!hillshade-classic_315.0_45.0_1.0_ffffffff",
            "00000000!hillshade-classic_315.0_45.0_1.0_ffffffff",
            "ffffffff!hillshade-igor_315.0_1.0_000000ff",
            "ffffffff!hillshade-igor_45.0_3.0_20408080",
            "ffffffff!slope-igor_1.0_000000ff",
            "ffffffff!slope-classic_60.0_2.0_000000ff",
            "ffffffff!color-relief_200.0_00ff00ff_400.0_ffff00ff_600.0_ff0000ff",
            "ffffffff!aspect_0.0_ff0000ff_3.14_00ff00ff_6.28_0000ffff",
            "ffffffff!hillshade-igor_315.0_1.0_000000ff!slope-igor_1.0_00000080!hillshade-classic_135.0_30.0_1.0_ffd00080",
            "000000ff!hillshade-classic~1.40~-0.10_315.0_45.0_1.0_ffffffff",
        ] {
            let shading = Shading::parse(s).unwrap();
            let fast = shading.render(&h, &w);
            let exact = reference(&shading, &h, &w);
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as i32;
            let mut worst = 0;

            for (i, e) in exact.iter().enumerate() {
                for (f, e) in [fast.r[i], fast.g[i], fast.b[i], fast.a[i]].iter().zip(e) {
                    worst = worst.max((q(*f) - q(*e)).abs());
                }
            }

            assert!(worst <= 1, "{s}: differs by {worst} levels");
        }
    }
}

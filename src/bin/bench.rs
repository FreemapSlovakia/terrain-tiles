//! Times each stage of a hillshade tile over the tiles of a z18 range:
//! `bench KEY=PATH... --x0 X --y0 Y --n N`.

use clap::Parser;
use image::{ImageEncoder, codecs::jpeg::JpegEncoder};
use std::{path::PathBuf, time::Instant};
use terrain_tiles::{
    elevation,
    encode::{self, Format},
    mosaic,
    shading::Shading,
    source::Source,
    tile::{TILE, TileWindow},
};

#[derive(Parser)]
struct Args {
    #[arg(required = true, value_parser = Source::parse_arg)]
    sources: Vec<(String, PathBuf)>,

    /// First z18 tile column and row of the range, and its side in tiles.
    #[arg(long)]
    x0: u32,
    #[arg(long)]
    y0: u32,
    #[arg(long)]
    n: u32,

    #[arg(
        long,
        default_value = "000000ff!hillshade-classic_315.0_45.0_1.0_ffffffff"
    )]
    shading: String,
}

#[derive(Default)]
struct Stat {
    ms: Vec<f64>,
    bytes: Vec<usize>,
}

impl Stat {
    fn time<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let t = Instant::now();
        let r = f();
        self.ms.push(t.elapsed().as_secs_f64() * 1000.0);
        r
    }

    fn encode(&mut self, f: impl FnOnce() -> Vec<u8>) {
        let b = self.time(f);
        self.bytes.push(b.len());
    }

    fn print(&mut self, name: &str) {
        if self.ms.is_empty() {
            println!("  {name:28} no samples");

            return;
        }

        self.ms.sort_by(f64::total_cmp);
        let med = |v: &[f64]| v[v.len() / 2];
        let mean = self.ms.iter().sum::<f64>() / self.ms.len() as f64;
        let size = if self.bytes.is_empty() {
            String::new()
        } else {
            self.bytes.sort();
            format!(
                "  {:6.1} KB",
                self.bytes[self.bytes.len() / 2] as f64 / 1024.0
            )
        };

        println!(
            "  {name:28} median {:7.3} ms  mean {mean:7.3} ms{size}",
            med(&self.ms)
        );
    }
}

/// The stats in first-use order, so the report follows the loop.
#[derive(Default)]
struct Stats(Vec<(&'static str, Stat)>);

impl Stats {
    fn get(&mut self, name: &'static str) -> &mut Stat {
        let i = match self.0.iter().position(|(n, _)| *n == name) {
            Some(i) => i,
            None => {
                self.0.push((name, Stat::default()));
                self.0.len() - 1
            }
        };

        &mut self.0[i].1
    }
}

fn main() {
    let args = Args::parse();

    let sources: Vec<Source> = args
        .sources
        .into_iter()
        .map(|(k, p)| Source::open(k, p, 1).expect("open source"))
        .collect();

    let shading = Shading::parse(&args.shading).expect("shading");
    let gray = shading.is_gray();

    for z in [18u8, 16, 14] {
        let f = 1u32 << (18 - z);
        let mut stats = Stats::default();
        let mut count = 0;

        for y in args.y0 / f..(args.y0 + args.n) / f {
            for x in args.x0 / f..(args.x0 + args.n) / f {
                let w = TileWindow::new(z, x, y, 18).unwrap();

                let Some(m) = stats
                    .get("read + mosaic")
                    .time(|| mosaic::read(&sources, &w, false).unwrap())
                else {
                    continue;
                };

                if m.heights.iter().filter(|h| h.is_nan()).count() > m.heights.len() / 10 {
                    continue; // mostly outside the data
                }

                count += 1;

                let planes = stats.get("shade").time(|| shading.render(&m.heights, &w));

                for (name, format) in [
                    ("served PNG", Format::Png),
                    ("served JPEG q90", Format::Jpeg),
                    ("served WebP q90", Format::Webp),
                ] {
                    stats
                        .get(name)
                        .encode(|| encode::encode(&planes, format, gray).unwrap().0);
                }

                // Alternatives to the served encoders.
                let rgb = planes.rgb_over_white();
                let s = TILE as u32;

                stats.get("JPEG q90 RGB (image)").encode(|| {
                    let mut b = Vec::new();
                    JpegEncoder::new_with_quality(&mut b, 90)
                        .write_image(&rgb, s, s, image::ExtendedColorType::Rgb8)
                        .unwrap();
                    b
                });

                for (name, q) in [("WebP q80 RGB", 80.0), ("WebP q90 RGB", 90.0)] {
                    stats
                        .get(name)
                        .encode(|| webp::Encoder::from_rgb(&rgb, s, s).encode(q).to_vec());
                }

                // Lower effort: method 0 is fastest, 4 is libwebp's default.
                for (name, method) in [("WebP q80 RGB method 0", 0), ("WebP q80 RGB method 2", 2)] {
                    stats.get(name).encode(|| {
                        let mut cfg = webp::WebPConfig::new().unwrap();
                        cfg.quality = 80.0;
                        cfg.method = method;
                        webp::Encoder::from_rgb(&rgb, s, s)
                            .encode_advanced(&cfg)
                            .unwrap()
                            .to_vec()
                    });
                }

                stats
                    .get("elevation 2 mm + zstd")
                    .encode(|| elevation::encode(&m.heights, z).unwrap());
            }
        }

        println!("z{z}: {count} tiles");

        for (name, stat) in &mut stats.0 {
            stat.print(name);
        }
    }
}

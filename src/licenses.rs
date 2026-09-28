//! What each source must be credited as, read from the
//! [elevation-sources](https://github.com/FreemapSlovakia/elevation-sources)
//! checkout the elevation API is credited from: a source key is the `name` of
//! the datasets it was built from.

use crate::error::AppError;
use axum::body::Bytes;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::ErrorKind, path::Path};

#[derive(Deserialize)]
struct SourceJson {
    name: String,
    attributions: Vec<Attribution>,
}

#[derive(Deserialize)]
struct Attribution {
    name: String,
    url: Option<String>,
}

/// One credit, as the web client's `/licenses` dictionaries hold it.
#[derive(Clone, PartialEq, Serialize)]
struct License {
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

/// `GET /licenses`: `shading:<key>` to the source's credits, the way the
/// outdoor renderer keys its own, so `X-Attribution: s<key>` resolves by it.
pub struct Licenses {
    pub body: Bytes,
    pub etag: String,
}

impl Licenses {
    /// Fails for a key no dataset names: serving uncredited data breaches its
    /// licence.
    pub fn load(dir: &Path, keys: &[&str]) -> Result<Self, AppError> {
        let io = |e: std::io::Error| AppError::Config(format!("{}: {e}", dir.display()));
        let mut by_name: BTreeMap<String, Vec<License>> = BTreeMap::new();

        // Sorted, so a name's credits follow the checkout's numbered order.
        let mut paths = std::fs::read_dir(dir)
            .map_err(io)?
            .map(|e| Ok(e?.path().join("source.json")))
            .collect::<Result<Vec<_>, std::io::Error>>()
            .map_err(io)?;

        paths.sort();

        for path in paths {
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                // Anything without one is not a dataset: `.git`, the README.
                Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                    continue;
                }
                // Skipping it could leave a credit out.
                Err(e) => return Err(AppError::Config(format!("{}: {e}", path.display()))),
            };

            let source: SourceJson = serde_json::from_str(&text)
                .map_err(|e| AppError::Config(format!("{}: {e}", path.display())))?;

            let credits = by_name.entry(source.name).or_default();

            for a in source.attributions {
                let license = License {
                    title: a.name,
                    url: a.url,
                };

                // Datasets sharing a name often share a credit, too.
                if !credits.contains(&license) {
                    credits.push(license);
                }
            }
        }

        let mut dict = BTreeMap::new();

        for key in keys {
            match by_name.get(*key) {
                Some(credits) if !credits.is_empty() => {
                    dict.insert(format!("shading:{key}"), credits.clone());
                }
                _ => {
                    return Err(AppError::Config(format!(
                        "no dataset in {} is named {key}, so nothing credits it",
                        dir.display()
                    )));
                }
            }
        }

        let body = serde_json::to_vec(&dict).map_err(|e| AppError::Config(e.to_string()))?;

        // FNV-1a: unlike std's hasher, stable across Rust releases.
        let hash = body.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
        });

        Ok(Self {
            etag: format!("\"{hash:016x}\""),
            body: body.into(),
        })
    }
}

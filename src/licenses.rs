//! The sources and their credits, read from the
//! [elevation-sources](https://github.com/FreemapSlovakia/elevation-sources)
//! checkout: every dataset directory that links a `mercator.tif` is a source,
//! ranked by its directory number and keyed by the rest of the name with `-`
//! as `_` (`090-es-29` → `es_29`), as `dem-pyramid` keys it.

use crate::error::AppError;
use axum::body::Bytes;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::ErrorKind,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
struct SourceJson {
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

pub struct Registry {
    /// Key and raster of each source, highest precedence first.
    pub sources: Vec<(String, PathBuf)>,
    pub licenses: Licenses,
}

/// `GET /licenses`: `shading:<key>` to the source's credits, the way the
/// outdoor renderer keys its own, so `X-Attribution: s<key>` resolves by it.
pub struct Licenses {
    pub body: Bytes,
    pub etag: String,
}

impl Registry {
    /// Fails for a source without credits: serving uncredited data breaches
    /// its licence.
    pub fn load(dir: &Path) -> Result<Self, AppError> {
        let config = |path: &Path, e: &dyn std::fmt::Display| AppError::Config(format!("{}: {e}", path.display()));

        let mut found = Vec::new();

        for entry in std::fs::read_dir(dir).map_err(|e| config(dir, &e))? {
            let path = entry.map_err(|e| config(dir, &e))?.path();
            let mercator = path.join("mercator.tif");

            // Anything without the link is not served: `.git`, the README, and
            // datasets only the elevation API reads.
            match std::fs::symlink_metadata(&mercator) {
                Ok(_) => {}
                Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => continue,
                Err(e) => return Err(config(&mercator, &e)),
            }

            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            let (number, slug) = name.split_at(name.find(|c: char| !c.is_ascii_digit()).unwrap_or(name.len()));

            let number = number
                .parse::<u32>()
                .map_err(|_| config(&path, &"the directory has no NNN precedence"))?;

            let key = slug
                .strip_prefix('-')
                .filter(|s| !s.is_empty())
                .ok_or_else(|| config(&path, &"the directory has no slug"))?
                .replace('-', "_");

            let json = path.join("source.json");
            let text = std::fs::read_to_string(&json).map_err(|e| config(&json, &e))?;
            let source: SourceJson = serde_json::from_str(&text).map_err(|e| config(&json, &e))?;

            let credits: Vec<License> = source
                .attributions
                .into_iter()
                .map(|a| License {
                    title: a.name,
                    url: a.url,
                })
                .collect();

            if credits.is_empty() {
                return Err(config(&json, &"no attributions, so nothing credits it"));
            }

            found.push((number, key, mercator, credits));
        }

        found.sort_by_key(|f| f.0);

        let dict: BTreeMap<String, &Vec<License>> =
            found.iter().map(|(_, key, _, credits)| (format!("shading:{key}"), credits)).collect();

        let body = serde_json::to_vec(&dict).map_err(|e| AppError::Config(e.to_string()))?;

        // FNV-1a: unlike std's hasher, stable across Rust releases.
        let hash = body.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
        });

        Ok(Self {
            sources: found.into_iter().map(|(_, key, path, _)| (key, path)).collect(),
            licenses: Licenses {
                etag: format!("\"{hash:016x}\""),
                body: body.into(),
            },
        })
    }
}

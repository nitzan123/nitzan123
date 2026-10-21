//! OpenStreetMap raster tiles for the map view, fetched with an identifying
//! User-Agent and kept in an on-disk cache (as the OSM tile usage policy
//! asks), so revisited areas work offline.

use crate::error::{Error, Result};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

const TILE_URL: &str = "https://tile.openstreetmap.org";
pub const MAX_ZOOM: u32 = 19;
/// Cached tiles are reused for this long before being refreshed.
const FRESH_FOR: Duration = Duration::from_secs(30 * 24 * 3600);
/// After a network failure, serve only from cache for a while.
const OFFLINE_BACKOFF: Duration = Duration::from_secs(120);

pub struct TileCache {
    dir: PathBuf,
    agent: ureq::Agent,
    offline_until: Mutex<Option<Instant>>,
}

impl TileCache {
    pub fn new(dir: PathBuf) -> Self {
        let agent = ureq::AgentBuilder::new()
            .user_agent(concat!(
                "PrismStudio/",
                env!("CARGO_PKG_VERSION"),
                " (desktop photo manager; map view of the user's photos)"
            ))
            .timeout(Duration::from_secs(10))
            .build();
        Self { dir, agent, offline_until: Mutex::new(None) }
    }

    fn path(&self, z: u32, x: u32, y: u32) -> PathBuf {
        self.dir.join(z.to_string()).join(x.to_string()).join(format!("{y}.png"))
    }

    /// PNG bytes of tile `z/x/y`: from cache when fresh, else downloaded;
    /// a stale cached copy is used when the network is unavailable.
    pub fn get(&self, z: u32, x: u32, y: u32) -> Result<Vec<u8>> {
        if z > MAX_ZOOM || x >= 1 << z || y >= 1 << z {
            return Err(Error::Invalid(format!("tile {z}/{x}/{y} out of range")));
        }
        let path = self.path(z, x, y);
        let cached = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
        let fresh = cached.and_then(|t| SystemTime::now().duration_since(t).ok()).is_some_and(|age| age < FRESH_FOR);
        if fresh {
            return Ok(std::fs::read(&path)?);
        }
        let offline = self.offline_until.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| Instant::now() < t);
        let fetched = if offline { Err(Error::Network("offline".into())) } else { self.fetch(z, x, y) };
        match fetched {
            Ok(bytes) => {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
                if std::fs::write(&tmp, &bytes).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
                Ok(bytes)
            }
            Err(e) => {
                if !offline {
                    *self.offline_until.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(Instant::now() + OFFLINE_BACKOFF);
                }
                if cached.is_some() {
                    Ok(std::fs::read(&path)?)
                } else {
                    Err(e)
                }
            }
        }
    }

    fn fetch(&self, z: u32, x: u32, y: u32) -> Result<Vec<u8>> {
        let resp =
            self.agent.get(&format!("{TILE_URL}/{z}/{x}/{y}.png")).call().map_err(|e| Error::Network(e.to_string()))?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut std::io::Read::take(resp.into_reader(), 2 << 20), &mut bytes)?;
        if !bytes.starts_with(b"\x89PNG") {
            return Err(Error::Network("tile server returned something other than a PNG".into()));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_out_of_range_and_serves_fresh_cache_offline() {
        let dir = tempfile::tempdir().unwrap();
        let t = TileCache::new(dir.path().to_path_buf());
        assert!(t.get(20, 0, 0).is_err());
        assert!(t.get(1, 2, 0).is_err());
        // A fresh cached tile is served without touching the network.
        let p = t.path(3, 4, 5);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"\x89PNGcached").unwrap();
        assert_eq!(t.get(3, 4, 5).unwrap(), b"\x89PNGcached");
    }
}

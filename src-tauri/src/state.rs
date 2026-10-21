use prism_core::raw::LinearImage;
use prism_core::thumbs::ThumbCache;
use prism_core::{Catalog, Result};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

/// Long edge of the image the Develop/Edit preview is rendered from.
pub const PREVIEW_SOURCE_DIM: u32 = 2048;
const SOURCE_CACHE_LEN: usize = 4;

/// Asset id + file size + mtime: a file edited outside the app is re-decoded.
type SourceKey = (i64, i64, i64);

/// Asset-independent lens database, loaded lazily from `<data>/lensfun`.
pub struct LensStore {
    pub dir: std::path::PathBuf,
    pub db: Mutex<Option<Arc<prism_core::lens::LensDb>>>,
}

impl LensStore {
    pub fn get(&self) -> Option<Arc<prism_core::lens::LensDb>> {
        let mut g = self.db.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() && self.dir.is_dir() {
            *g = prism_core::lens::LensDb::load_dir(&self.dir).ok().map(Arc::new);
        }
        g.clone()
    }
}

pub struct AppState {
    catalog: Mutex<Catalog>,
    pub thumbs: ThumbCache,
    pub geocoder: prism_core::geocode::Geocoder,
    pub tiles: prism_core::tiles::TileCache,
    pub lenses: LensStore,
    pub ai: prism_core::ai::Models,
    pub ai_running: std::sync::atomic::AtomicBool,
    pub ai_cancel: std::sync::atomic::AtomicBool,
    /// Recently decoded linear sources keyed by asset id, so slider drags
    /// don't re-decode the file.
    sources: Mutex<VecDeque<(SourceKey, Arc<LinearImage>)>>,
}

impl AppState {
    pub fn open(data_dir: &Path, cache_dir: &Path) -> Result<Self> {
        Ok(Self {
            catalog: Mutex::new(Catalog::open(&data_dir.join("catalog.sqlite"))?),
            thumbs: ThumbCache::new(cache_dir.join("thumbs"))?,
            geocoder: prism_core::geocode::Geocoder::new(),
            tiles: prism_core::tiles::TileCache::new(cache_dir.join("tiles")),
            lenses: LensStore { dir: data_dir.join("lensfun"), db: Mutex::new(None) },
            ai: prism_core::ai::Models::new(data_dir.join("ai")),
            ai_running: Default::default(),
            ai_cancel: Default::default(),
            sources: Mutex::new(VecDeque::new()),
        })
    }

    pub fn catalog(&self) -> MutexGuard<'_, Catalog> {
        self.catalog.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn source(&self, id: i64, path: &Path) -> Result<Arc<LinearImage>> {
        let (size, mtime) = prism_core::scan::stat(path).unwrap_or((-1, -1));
        let key = (id, size, mtime);
        {
            let cache = self.sources.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, img)) = cache.iter().find(|(k, _)| *k == key) {
                return Ok(img.clone());
            }
        }
        let img = Arc::new(prism_core::imaging::load_linear(path, PREVIEW_SOURCE_DIM)?);
        let mut cache = self.sources.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|(k, _)| k.0 != id);
        cache.push_front((key, img.clone()));
        cache.truncate(SOURCE_CACHE_LEN);
        Ok(img)
    }
}

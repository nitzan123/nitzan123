use prism_core::raw::LinearImage;
use prism_core::thumbs::ThumbCache;
use prism_core::{Catalog, Result};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

/// Long edge of the image the Develop/Edit preview is rendered from.
pub const PREVIEW_SOURCE_DIM: u32 = 2048;
const SOURCE_CACHE_LEN: usize = 4;

pub struct AppState {
    catalog: Mutex<Catalog>,
    pub thumbs: ThumbCache,
    /// Recently decoded linear sources keyed by asset id, so slider drags
    /// don't re-decode the file.
    sources: Mutex<VecDeque<(i64, Arc<LinearImage>)>>,
}

impl AppState {
    pub fn open(data_dir: &Path, cache_dir: &Path) -> Result<Self> {
        Ok(Self {
            catalog: Mutex::new(Catalog::open(&data_dir.join("catalog.sqlite"))?),
            thumbs: ThumbCache::new(cache_dir.join("thumbs"))?,
            sources: Mutex::new(VecDeque::new()),
        })
    }

    pub fn catalog(&self) -> MutexGuard<'_, Catalog> {
        self.catalog.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn source(&self, id: i64, path: &Path) -> Result<Arc<LinearImage>> {
        {
            let cache = self.sources.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, img)) = cache.iter().find(|(i, _)| *i == id) {
                return Ok(img.clone());
            }
        }
        let img = Arc::new(prism_core::imaging::load_linear(path, PREVIEW_SOURCE_DIM)?);
        let mut cache = self.sources.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|(i, _)| *i != id);
        cache.push_front((id, img.clone()));
        cache.truncate(SOURCE_CACHE_LEN);
        Ok(img)
    }
}

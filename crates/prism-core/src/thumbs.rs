//! On-disk thumbnail cache. Key = BLAKE3(path, mtime, size), so edits to the
//! original file invalidate automatically and the cache can be wiped safely.

use crate::error::Result;
use crate::imaging::load_display;
use image::RgbImage;
use std::fs;
use std::path::{Path, PathBuf};

pub const THUMB_SIZE: u32 = 360;

#[derive(Debug, Clone)]
pub struct ThumbCache {
    dir: PathBuf,
}

impl ThumbCache {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn key(path: &Path, mtime: i64, size: i64) -> String {
        let mut h = blake3::Hasher::new();
        h.update(path.to_string_lossy().as_bytes());
        h.update(&mtime.to_le_bytes());
        h.update(&size.to_le_bytes());
        h.finalize().to_hex()[..32].to_string()
    }

    pub fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(&key[..2]).join(format!("{key}.jpg"))
    }

    /// Returns the cached thumbnail file, generating it if needed. Also
    /// returns the decoded thumbnail when it had to be generated so callers
    /// can reuse it (e.g. for hashing) without decoding twice.
    pub fn ensure(&self, src: &Path, mtime: i64, size: i64) -> Result<(PathBuf, Option<RgbImage>)> {
        let target = self.path_for(&Self::key(src, mtime, size));
        if target.exists() {
            return Ok((target, None));
        }
        let img = load_display(src, THUMB_SIZE)?;
        self.write(&target, &img)?;
        Ok((target, Some(img)))
    }

    /// Like [`ensure`](Self::ensure) for a rendered variant of the file
    /// (e.g. with develop settings applied). `variant` must uniquely
    /// describe the rendering; `make` is only called on a cache miss.
    pub fn ensure_variant(
        &self,
        src: &Path,
        mtime: i64,
        size: i64,
        variant: &str,
        make: impl FnOnce() -> Result<RgbImage>,
    ) -> Result<PathBuf> {
        let mut h = blake3::Hasher::new();
        h.update(Self::key(src, mtime, size).as_bytes());
        h.update(variant.as_bytes());
        let target = self.path_for(&h.finalize().to_hex()[..32]);
        if !target.exists() {
            self.write(&target, &make()?)?;
        }
        Ok(target)
    }

    fn write(&self, target: &Path, img: &RgbImage) -> Result<()> {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        // Unique temp name: the indexer and the image protocol may build the
        // same thumbnail concurrently; each must rename its own complete file.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = target.with_extension(format!("{}-{n}.tmp", std::process::id()));
        {
            let mut f = std::io::BufWriter::new(fs::File::create(&tmp)?);
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut f, 82).encode_image(img)?;
        }
        fs::rename(&tmp, target)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_once_and_reuses() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("p.png");
        RgbImage::from_pixel(1000, 500, image::Rgb([10, 20, 30])).save(&src).unwrap();
        let cache = ThumbCache::new(dir.path().join("cache")).unwrap();
        let (p1, fresh) = cache.ensure(&src, 1, 2).unwrap();
        assert!(p1.exists());
        let fresh = fresh.unwrap();
        assert_eq!(fresh.width(), THUMB_SIZE);
        let (p2, again) = cache.ensure(&src, 1, 2).unwrap();
        assert_eq!(p1, p2);
        assert!(again.is_none());
        assert_ne!(ThumbCache::key(&src, 1, 2), ThumbCache::key(&src, 3, 2));

        let mut calls = 0;
        let mut make = || {
            calls += 1;
            Ok(RgbImage::new(8, 8))
        };
        let v1 = cache.ensure_variant(&src, 1, 2, "a", &mut make).unwrap();
        let v1b = cache.ensure_variant(&src, 1, 2, "a", &mut make).unwrap();
        let v2 = cache.ensure_variant(&src, 1, 2, "b", &mut make).unwrap();
        assert_eq!(v1, v1b);
        assert_ne!(v1, v2);
        assert_ne!(v1, p1);
        assert_eq!(calls, 2);
    }
}

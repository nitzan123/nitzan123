//! Folder discovery. Pure filesystem + EXIF; the catalog decides what to do
//! with the results.

use crate::imaging::{ext_of, is_supported};
use crate::metadata::{read_exif, ExifInfo};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub folder: PathBuf,
    pub file_name: String,
    pub ext: String,
    pub size: i64,
    /// Seconds since the Unix epoch.
    pub mtime: i64,
    pub exif: ExifInfo,
}

pub fn list_media(root: &Path, recursive: bool) -> Vec<PathBuf> {
    let walker = WalkDir::new(root).follow_links(false).max_depth(if recursive { usize::MAX } else { 1 });
    walker
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.into_path())
        .filter(|p| is_supported(p))
        .collect()
}

pub fn stat(path: &Path) -> Option<(i64, i64)> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((md.len() as i64, mtime))
}

pub fn scan_files(paths: &[PathBuf]) -> Vec<ScannedFile> {
    paths
        .par_iter()
        .filter_map(|p| {
            let (size, mtime) = stat(p)?;
            let mut exif = read_exif(p);
            // Many RAW formats keep a small preview in IFD0, so its
            // dimensions are not the sensor's. Don't record implausible ones.
            if crate::raw::is_raw_ext(&ext_of(p)) && exif.width.max(exif.height).unwrap_or(0) < 1000 {
                exif.width = None;
                exif.height = None;
            }
            Some(ScannedFile {
                folder: p.parent().map(Path::to_path_buf).unwrap_or_default(),
                file_name: p.file_name()?.to_string_lossy().into_owned(),
                ext: ext_of(p),
                size,
                mtime,
                exif,
                path: p.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_supported_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        image::RgbImage::new(4, 4).save(dir.path().join("a.png")).unwrap();
        image::RgbImage::new(4, 4).save(sub.join("b.jpg")).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        std::fs::write(dir.path().join(".hidden.jpg"), "x").unwrap();

        assert_eq!(list_media(dir.path(), false).len(), 1);
        let all = list_media(dir.path(), true);
        assert_eq!(all.len(), 2);
        let scanned = scan_files(&all);
        assert!(scanned.iter().any(|s| s.file_name == "b.jpg" && s.ext == "jpg"));
    }
}

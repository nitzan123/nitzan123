//! Turns a burst of file-system change notifications into the smallest set
//! of catalog jobs: re-index a folder (just its files, or everything below
//! a newly created folder) or prune entries under a path that vanished.
//!
//! The watcher itself lives in the app shell; this part is pure so it can
//! be tested without a real file-system watcher.

use crate::imaging::is_supported;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Job {
    /// Re-scan `dir` (and everything below it when `recursive`).
    Index { dir: PathBuf, recursive: bool },
    /// Drop catalog entries at or below `path` whose files are gone.
    Prune { path: PathBuf },
}

/// Files the app writes itself or that are never photos.
fn ignored(p: &Path) -> bool {
    let name = p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    name.starts_with('.') || name.ends_with(".xmp") || name.ends_with(".tmp") || name.ends_with('~')
}

fn inside(p: &Path, root: &Path) -> bool {
    p != root && p.starts_with(root)
}

/// Plans jobs for the changed `paths` (as reported by the watcher).
pub fn plan(paths: impl IntoIterator<Item = PathBuf>) -> Vec<Job> {
    let mut index_rec = BTreeSet::new();
    let mut index_flat = BTreeSet::new();
    let mut prune = BTreeSet::new();
    for p in paths {
        if ignored(&p) || p.to_str().is_none() {
            continue;
        }
        match std::fs::metadata(&p) {
            Ok(m) if m.is_dir() => {
                index_rec.insert(p);
            }
            Ok(_) if is_supported(&p) => {
                if let Some(parent) = p.parent() {
                    index_flat.insert(parent.to_path_buf());
                }
            }
            Ok(_) => {}
            // Gone: a deleted photo or a deleted/renamed-away folder.
            Err(_) => {
                if is_supported(&p) {
                    if let Some(parent) = p.parent() {
                        index_flat.insert(parent.to_path_buf());
                    }
                } else {
                    prune.insert(p);
                }
            }
        }
    }
    let covered = |p: &Path| index_rec.iter().any(|r: &PathBuf| p == r || inside(p, r));
    let mut jobs: Vec<Job> = Vec::new();
    for dir in &index_rec {
        if !index_rec.iter().any(|r| inside(dir, r)) {
            jobs.push(Job::Index { dir: dir.clone(), recursive: true });
        }
    }
    for dir in index_flat {
        if !covered(&dir) {
            jobs.push(Job::Index { dir, recursive: false });
        }
    }
    for path in &prune {
        if !prune.iter().any(|r| inside(path, r)) {
            jobs.push(Job::Prune { path: path.clone() });
        }
    }
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_minimal_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join("new/deeper")).unwrap();
        std::fs::create_dir_all(d.join("old")).unwrap();
        for f in ["old/a.jpg", "old/b.NEF", "new/deeper/c.jpg", "old/notes.txt", "old/a.jpg.xmp", "old/.hidden.jpg"] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        let jobs = plan([
            d.join("old/a.jpg"),
            d.join("old/b.NEF"),
            d.join("old/notes.txt"),    // not a photo
            d.join("old/a.jpg.xmp"),    // our own sidecar
            d.join("old/.hidden.jpg"),  // temp/hidden
            d.join("new"),              // new folder: index recursively…
            d.join("new/deeper"),       // …which covers this
            d.join("new/deeper/c.jpg"), // …and this
            d.join("old/deleted.jpg"),  // deleted photo → re-index its folder
            d.join("gone"),             // deleted folder → prune
            d.join("gone/sub"),         // covered by the prune above
        ]);
        assert_eq!(
            jobs,
            vec![
                Job::Index { dir: d.join("new"), recursive: true },
                Job::Index { dir: d.join("old"), recursive: false },
                Job::Prune { path: d.join("gone") },
            ]
        );
        assert!(plan([d.join("old/a.jpg.xmp")]).is_empty());
    }
}

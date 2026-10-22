//! Keeps the catalog in sync with library folders while the app runs:
//! photos copied in, edited elsewhere, renamed or deleted show up without
//! a manual rescan. Notifications are collected until the folder has been
//! quiet for a moment (a camera import fires thousands), then turned into
//! a few index/prune jobs by `prism_core::watch::plan`.

use crate::indexer;
use crate::state::AppState;
use notify::{RecursiveMode, Watcher as _};
use prism_core::watch::{plan, Job};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// How long a folder must be quiet before changes are processed.
const SETTLE: Duration = Duration::from_millis(1500);

pub struct Watch {
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    watched: Mutex<HashSet<PathBuf>>,
}

impl Watch {
    /// Starts the watcher thread and watches every library root.
    pub fn start(app: &AppHandle) -> Self {
        let (tx, rx) = channel::<PathBuf>();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if !matches!(ev.kind, notify::EventKind::Access(_)) {
                    for p in ev.paths {
                        let _ = tx.send(p);
                    }
                }
            }
        })
        .map_err(|e| eprintln!("folder watching unavailable: {e}"))
        .ok();
        let handle = app.clone();
        std::thread::Builder::new()
            .name("prism-watch".into())
            .spawn(move || {
                let mut pending: Vec<PathBuf> = Vec::new();
                loop {
                    match rx.recv_timeout(SETTLE) {
                        Ok(p) => pending.push(p),
                        Err(RecvTimeoutError::Timeout) if !pending.is_empty() => {
                            process(&handle, plan(std::mem::take(&mut pending)));
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            })
            .expect("watch thread");
        let w = Self { watcher: Mutex::new(watcher), watched: Mutex::new(HashSet::new()) };
        if let Ok(roots) = app.state::<AppState>().catalog().roots() {
            for (root, recursive) in roots {
                w.add(Path::new(&root), recursive);
            }
        }
        w
    }

    /// Watches a library folder (no-op if already watched or missing).
    pub fn add(&self, root: &Path, recursive: bool) {
        if !root.is_dir() || !self.watched.lock().unwrap_or_else(|e| e.into_inner()).insert(root.to_path_buf()) {
            return;
        }
        if let Some(w) = self.watcher.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let mode = if recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
            if let Err(e) = w.watch(root, mode) {
                eprintln!("cannot watch {}: {e}", root.display());
            }
        }
    }
}

fn process(app: &AppHandle, jobs: Vec<Job>) {
    let state = app.state::<AppState>();
    let mut changes = 0;
    for job in jobs {
        changes += match job {
            Job::Index { dir, recursive } => indexer::run(app, &dir, recursive, true).unwrap_or(0),
            Job::Prune { path } => state.catalog().prune_under(&path.to_string_lossy(), true).unwrap_or(0),
        };
    }
    if changes > 0 {
        let _ = app.emit("library-watch", changes);
    }
}

//! Photos and folders handed to the app by the operating system: on the
//! command line, by a second launch (forwarded to the running window), or
//! via macOS "open document" events. Folders join the library; a photo's
//! folder joins it too and the photo is shown once it is indexed.

use crate::commands;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

/// A photo requested before the UI was ready to receive events.
#[derive(Default)]
pub struct PendingOpen(pub Mutex<Option<String>>);

/// Adds `paths` to the library and asks the UI to show the last photo.
/// `startup`: the UI isn't listening yet, so the photo is parked for it.
pub fn handle(app: &AppHandle, paths: impl IntoIterator<Item = PathBuf>, startup: bool) {
    let mut photo = None;
    for p in paths {
        let Ok(p) = std::fs::canonicalize(&p) else { continue };
        let Some(text) = p.to_str().map(str::to_string) else { continue };
        if p.is_dir() {
            let _ = commands::add_folder(app.clone(), text, true);
        } else if prism_core::imaging::is_supported(&p) {
            if let Some(parent) = p.parent().and_then(Path::to_str) {
                let _ = commands::add_folder(app.clone(), parent.to_string(), false);
            }
            photo = Some(text);
        }
    }
    let Some(photo) = photo else { return };
    if startup {
        *app.state::<PendingOpen>().0.lock().unwrap_or_else(|e| e.into_inner()) = Some(photo);
    } else {
        let _ = app.emit("open-file", photo);
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Command-line arguments (relative paths resolved against `cwd`).
pub fn from_args(args: &[String], cwd: &Path) -> Vec<PathBuf> {
    args.iter()
        .filter(|a| !a.starts_with('-'))
        .map(|a| {
            let p = PathBuf::from(a);
            if p.is_absolute() {
                p
            } else {
                cwd.join(p)
            }
        })
        .collect()
}

//! Background indexing: scan → catalog upsert → sidecar import → thumbnail +
//! perceptual hash. Runs on a worker thread and reports progress as
//! `index-progress` events so the UI never blocks.

use crate::state::AppState;
use prism_core::catalog::Upsert;
use prism_core::{scan, similarity, xmp};
use rayon::prelude::*;
use serde::Serialize;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub folder: String,
    pub stage: &'static str,
    pub done: usize,
    pub total: usize,
}

pub fn spawn(app: AppHandle, root: PathBuf, recursive: bool) {
    std::thread::spawn(move || {
        if let Err(e) = run(&app, &root, recursive) {
            let _ = app.emit("index-error", e.to_string());
        }
    });
}

fn run(app: &AppHandle, root: &std::path::Path, recursive: bool) -> prism_core::Result<()> {
    let state = app.state::<AppState>();
    let folder = root.to_string_lossy().to_string();
    let emit = |stage, done, total| {
        let _ = app.emit("index-progress", Progress { folder: folder.clone(), stage, done, total });
    };

    emit("scanning", 0, 0);
    let paths = scan::list_media(root, recursive);
    let scanned = scan::scan_files(&paths);
    let results = state.catalog().upsert_many(&scanned)?;

    // Import ratings/keywords/develop from sidecars of new files. Disk work
    // (reading sidecars, checking for deleted files) happens before taking
    // the catalog lock so the UI stays responsive on slow/network drives.
    let sidecars: Vec<(i64, xmp::Sidecar)> = scanned
        .iter()
        .zip(&results)
        .filter_map(|(file, res)| match res {
            Upsert::Inserted(id) => xmp::read_sidecar(&file.path).map(|sc| (*id, sc)),
            _ => None,
        })
        .collect();
    let known = state.catalog().assets_under(&folder, recursive)?;
    let gone: Vec<i64> =
        known.into_iter().filter(|(_, p)| !std::path::Path::new(p).exists()).map(|(id, _)| id).collect();
    {
        let cat = state.catalog();
        for &(id, ref sc) in &sidecars {
            cat.set_rating(&[id], sc.rating)?;
            cat.set_color_label(&[id], sc.color_label)?;
            cat.set_tagged(&[id], sc.tagged)?;
            if let Some(c) = &sc.caption {
                cat.set_caption(id, c)?;
            }
            for k in &sc.keywords {
                cat.add_keyword(&[id], k)?;
            }
            cat.set_develop(id, sc.develop.as_ref())?;
        }
        // Files and whole sub-folders deleted from disk since the last scan.
        cat.delete_assets(&gone)?;
    }
    emit("indexed", scanned.len(), scanned.len());

    // Thumbnails + hashes in parallel; DB writes batched back on this thread.
    let need_hash: std::collections::HashSet<i64> = state.catalog().missing_phash()?.into_iter().collect();
    let total = scanned.len();
    let done = std::sync::atomic::AtomicUsize::new(0);
    let hashes: Vec<(i64, u64)> = scanned
        .par_iter()
        .zip(results.par_iter())
        .filter_map(|(f, r)| {
            let id = r.id();
            let out = state.thumbs.ensure(&f.path, f.mtime, f.size).ok();
            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if n.is_multiple_of(16) || n == total {
                emit("thumbnails", n, total);
            }
            if !need_hash.contains(&id) {
                return None;
            }
            let img = match out? {
                (_, Some(img)) => img,
                (path, None) => image::open(path).ok()?.to_rgb8(),
            };
            Some((id, similarity::dhash(&img)))
        })
        .collect();
    let cat = state.catalog();
    for (id, h) in hashes {
        cat.set_phash(id, h)?;
    }
    drop(cat);
    emit("done", total, total);
    Ok(())
}

/// Reads GPS for catalog entries created before positions were stored, in
/// the background, then tells the UI so the map can refresh.
pub fn spawn_gps_backfill(app: AppHandle) {
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let mut found = 0usize;
        loop {
            let Ok(batch) = state.catalog().gps_pending(256) else { return };
            if batch.is_empty() {
                break;
            }
            let read: Vec<(i64, Option<[f64; 2]>)> = batch
                .par_iter()
                .map(|(id, p)| (*id, prism_core::metadata::read_exif(std::path::Path::new(p)).gps))
                .collect();
            let cat = state.catalog();
            for (id, gps) in read {
                found += gps.is_some() as usize;
                if cat.set_gps(id, gps).is_err() {
                    return;
                }
            }
        }
        if found > 0 {
            let _ = app.emit("gps-backfilled", found);
        }
    });
}

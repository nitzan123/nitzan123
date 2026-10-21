//! Background AI analysis: CLIP embedding + keyword suggestions + faces for
//! every photo that hasn't been analysed (or changed since), then face
//! clustering into people. Progress: `ai-progress` events; cancellable.

use crate::state::AppState;
use prism_core::ai::{self, LabelIndex};
use prism_core::catalog::Analysis;
use rayon::prelude::*;
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub stage: &'static str,
    pub done: usize,
    pub total: usize,
}

/// Long edge of the image analysed (faces down to ~20 px still detected).
const ANALYSIS_DIM: u32 = 1280;

pub fn spawn(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    if state.ai_running.swap(true, Ordering::SeqCst) {
        return Err("AI analysis is already running".into());
    }
    state.ai_cancel.store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let res = run(&app, &state);
        state.ai_running.store(false, Ordering::SeqCst);
        match res {
            Ok(n) => {
                let _ = app.emit("ai-progress", Progress { stage: "done", done: n, total: n });
            }
            Err(e) => {
                let _ = app.emit("ai-error", e);
            }
        }
    });
    Ok(())
}

fn run(app: &AppHandle, state: &AppState) -> Result<usize, String> {
    let emit = |stage, done, total| {
        let _ = app.emit("ai-progress", Progress { stage, done, total });
    };
    emit("loading", 0, 0);
    let labels = LabelIndex::load_or_build(&state.ai).map_err(|e| e.to_string())?;
    let pending = state.catalog().ai_pending().map_err(|e| e.to_string())?;
    let total = pending.len();
    let done = AtomicUsize::new(0);
    // Analyse in chunks so results are stored (and survive cancel) as we go.
    for chunk in pending.chunks(16) {
        if state.ai_cancel.load(Ordering::SeqCst) {
            break;
        }
        let results: Vec<Analysis> = chunk
            .par_iter()
            .filter_map(|(id, path, mtime)| {
                let img = prism_core::imaging::load_display(Path::new(path), ANALYSIS_DIM).ok();
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                emit("analyzing", n, total);
                // Unreadable files are marked analysed (empty) so they aren't retried forever.
                let Some(img) = img else {
                    return Some(Analysis { asset_id: *id, mtime: *mtime, ..Default::default() });
                };
                let (embedding, tags, faces) = ai::analyze(&state.ai, &labels, &img).ok()?;
                Some(Analysis { asset_id: *id, mtime: *mtime, embedding: Some(embedding), tags, faces })
            })
            .collect();
        let mut cat = state.catalog();
        for a in &results {
            cat.store_analysis(a).map_err(|e| e.to_string())?;
        }
    }
    emit("people", total, total);
    state.catalog().cluster_faces(ai::faces::SAME_PERSON).map_err(|e| e.to_string())?;
    Ok(done.load(Ordering::Relaxed))
}

//! Commands invoked from the web UI. Errors cross the bridge as strings.
//! Heavy work (rendering, exporting) runs on the blocking pool.

use crate::indexer;
use crate::state::AppState;
use base64::Engine;
use prism_core::catalog::{FolderInfo, NamedCount, SavedSearch};
use prism_core::develop::{
    auto::{auto_settings, auto_white_balance},
    render_to_rgb8,
};
use prism_core::{xmp, Asset, Catalog, DevelopSettings, SearchQuery};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, State};

type CmdResult<T> = Result<T, String>;

fn s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

pub fn encode_jpeg(img: &image::RgbImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode_image(img).map_err(s)?;
    Ok(out)
}

/// Mirrors catalog state for `ids` into XMP sidecars. Failures (read-only
/// media etc.) are ignored: the catalog stays the source of truth.
fn sync_sidecars(cat: &Catalog, ids: &[i64]) {
    for &id in ids {
        let (Ok(a), Ok(keywords), Ok(develop)) = (cat.asset(id), cat.keywords_of(id), cat.develop(id)) else {
            continue;
        };
        let sc = xmp::Sidecar {
            rating: a.rating,
            color_label: a.color_label,
            tagged: a.tagged,
            caption: a.caption.clone(),
            keywords,
            develop,
        };
        let _ = xmp::write_sidecar(Path::new(&a.path), &sc);
    }
}

// ------------------------------------------------------------------ library

#[tauri::command]
pub fn add_folder(app: AppHandle, path: String, recursive: bool) -> CmdResult<()> {
    let p = PathBuf::from(&path);
    if !p.is_dir() {
        return Err(format!("not a folder: {path}"));
    }
    indexer::spawn(app, p, recursive);
    Ok(())
}

#[tauri::command]
pub fn rescan_folder(app: AppHandle, path: String) -> CmdResult<()> {
    add_folder(app, path, false)
}

#[tauri::command]
pub fn folders(state: State<'_, AppState>) -> CmdResult<Vec<FolderInfo>> {
    state.catalog().folders().map_err(s)
}

#[tauri::command]
pub fn search(state: State<'_, AppState>, query: SearchQuery) -> CmdResult<Vec<Asset>> {
    state.catalog().search(&query).map_err(s)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetDetails {
    asset: Asset,
    keywords: Vec<String>,
}

#[tauri::command]
pub fn asset_details(state: State<'_, AppState>, id: i64) -> CmdResult<AssetDetails> {
    let cat = state.catalog();
    Ok(AssetDetails { asset: cat.asset(id).map_err(s)?, keywords: cat.keywords_of(id).map_err(s)? })
}

#[tauri::command]
pub fn set_rating(state: State<'_, AppState>, ids: Vec<i64>, rating: u8) -> CmdResult<()> {
    let cat = state.catalog();
    cat.set_rating(&ids, rating).map_err(s)?;
    sync_sidecars(&cat, &ids);
    Ok(())
}

#[tauri::command]
pub fn set_color_label(state: State<'_, AppState>, ids: Vec<i64>, label: u8) -> CmdResult<()> {
    let cat = state.catalog();
    cat.set_color_label(&ids, label).map_err(s)?;
    sync_sidecars(&cat, &ids);
    Ok(())
}

#[tauri::command]
pub fn set_tagged(state: State<'_, AppState>, ids: Vec<i64>, tagged: bool) -> CmdResult<()> {
    let cat = state.catalog();
    cat.set_tagged(&ids, tagged).map_err(s)?;
    sync_sidecars(&cat, &ids);
    Ok(())
}

#[tauri::command]
pub fn set_caption(state: State<'_, AppState>, id: i64, caption: String) -> CmdResult<()> {
    let cat = state.catalog();
    cat.set_caption(id, &caption).map_err(s)?;
    sync_sidecars(&cat, &[id]);
    Ok(())
}

#[tauri::command]
pub fn add_keyword(state: State<'_, AppState>, ids: Vec<i64>, keyword: String) -> CmdResult<()> {
    let cat = state.catalog();
    cat.add_keyword(&ids, &keyword).map_err(s)?;
    sync_sidecars(&cat, &ids);
    Ok(())
}

#[tauri::command]
pub fn remove_keyword(state: State<'_, AppState>, ids: Vec<i64>, keyword: String) -> CmdResult<()> {
    let cat = state.catalog();
    cat.remove_keyword(&ids, &keyword).map_err(s)?;
    sync_sidecars(&cat, &ids);
    Ok(())
}

#[tauri::command]
pub fn keywords(state: State<'_, AppState>) -> CmdResult<Vec<NamedCount>> {
    state.catalog().keywords().map_err(s)
}

#[tauri::command]
pub fn collections(state: State<'_, AppState>) -> CmdResult<Vec<NamedCount>> {
    state.catalog().collections().map_err(s)
}

#[tauri::command]
pub fn create_collection(state: State<'_, AppState>, name: String) -> CmdResult<i64> {
    state.catalog().create_collection(&name).map_err(s)
}

#[tauri::command]
pub fn add_to_collection(state: State<'_, AppState>, collection_id: i64, ids: Vec<i64>) -> CmdResult<()> {
    state.catalog().add_to_collection(collection_id, &ids).map_err(s)
}

#[tauri::command]
pub fn remove_from_collection(state: State<'_, AppState>, collection_id: i64, ids: Vec<i64>) -> CmdResult<()> {
    state.catalog().remove_from_collection(collection_id, &ids).map_err(s)
}

#[tauri::command]
pub fn saved_searches(state: State<'_, AppState>) -> CmdResult<Vec<SavedSearch>> {
    state.catalog().saved_searches().map_err(s)
}

#[tauri::command]
pub fn save_search(state: State<'_, AppState>, name: String, query: SearchQuery) -> CmdResult<i64> {
    state.catalog().save_search(&name, &query).map_err(s)
}

#[tauri::command]
pub fn delete_saved_search(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    state.catalog().delete_saved_search(id).map_err(s)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarHit {
    asset: Asset,
    distance: u32,
}

#[tauri::command]
pub fn similar(state: State<'_, AppState>, id: i64, max_distance: Option<u32>) -> CmdResult<Vec<SimilarHit>> {
    Ok(state
        .catalog()
        .similar(id, max_distance.unwrap_or(10))
        .map_err(s)?
        .into_iter()
        .map(|(asset, distance)| SimilarHit { asset, distance })
        .collect())
}

// ------------------------------------------------------------------ develop

#[tauri::command]
pub fn develop_get(state: State<'_, AppState>, id: i64) -> CmdResult<DevelopSettings> {
    Ok(state.catalog().develop(id).map_err(s)?.unwrap_or_default())
}

#[tauri::command]
pub fn develop_save(state: State<'_, AppState>, id: i64, settings: DevelopSettings) -> CmdResult<()> {
    let cat = state.catalog();
    cat.set_develop(id, Some(&settings)).map_err(s)?;
    sync_sidecars(&cat, &[id]);
    Ok(())
}

#[tauri::command]
pub async fn develop_auto(app: AppHandle, id: i64) -> CmdResult<DevelopSettings> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let path = state.catalog().asset(id).map_err(s)?.path;
        let src = state.source(id, Path::new(&path)).map_err(s)?;
        Ok(auto_settings(&src))
    })
    .await
    .map_err(s)?
}

/// Conservative automatic white balance: `[temperature, tint]`.
#[tauri::command]
pub async fn develop_auto_wb(app: AppHandle, id: i64) -> CmdResult<[f32; 2]> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let path = state.catalog().asset(id).map_err(s)?.path;
        let src = state.source(id, Path::new(&path)).map_err(s)?;
        let (t, n) = auto_white_balance(&src);
        Ok([t, n])
    })
    .await
    .map_err(s)?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    data_url: String,
    width: u32,
    height: u32,
    /// 3 × 64 bins (R, G, B).
    histogram: Vec<u32>,
}

#[tauri::command]
pub async fn develop_preview(app: AppHandle, id: i64, settings: DevelopSettings, max: u32) -> CmdResult<Preview> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let path = state.catalog().asset(id).map_err(s)?.path;
        let mut src = state.source(id, Path::new(&path)).map_err(s)?;
        let max = max.clamp(256, crate::state::PREVIEW_SOURCE_DIM);
        if src.width().max(src.height()) > max {
            let k = max as f32 / src.width().max(src.height()) as f32;
            let (w, h) = ((src.width() as f32 * k) as u32, (src.height() as f32 * k) as u32);
            src = std::sync::Arc::new(image::imageops::resize(
                &*src,
                w.max(1),
                h.max(1),
                image::imageops::FilterType::Triangle,
            ));
        }
        let out = render_to_rgb8(&src, &settings);
        let mut histogram = vec![0u32; 192];
        for p in out.pixels() {
            for c in 0..3 {
                histogram[c * 64 + (p.0[c] as usize >> 2)] += 1;
            }
        }
        let jpeg = encode_jpeg(&out, 90)?;
        Ok(Preview {
            data_url: format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(jpeg)),
            width: out.width(),
            height: out.height(),
            histogram,
        })
    })
    .await
    .map_err(s)?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    written: Vec<String>,
    failed: Vec<String>,
}

/// Renders each asset at full (or `max_dim`) resolution to `dir`.
#[tauri::command]
pub async fn export(app: AppHandle, ids: Vec<i64>, dir: String, quality: u8, max_dim: u32) -> CmdResult<ExportResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).map_err(s)?;
        let mut res = ExportResult { written: Vec::new(), failed: Vec::new() };
        for id in ids {
            let job = || -> Result<PathBuf, String> {
                let (asset, settings) = {
                    let cat = state.catalog();
                    (cat.asset(id).map_err(s)?, cat.develop(id).map_err(s)?.unwrap_or_default())
                };
                let src = prism_core::imaging::load_linear(Path::new(&asset.path), max_dim).map_err(s)?;
                let img = render_to_rgb8(&src, &settings);
                let stem = Path::new(&asset.file_name).file_stem().unwrap_or_default().to_string_lossy().to_string();
                let mut target = dir.join(format!("{stem}.jpg"));
                let mut n = 1;
                while target.exists() {
                    target = dir.join(format!("{stem}-{n}.jpg"));
                    n += 1;
                }
                std::fs::write(&target, encode_jpeg(&img, quality.clamp(10, 100))?).map_err(s)?;
                Ok(target)
            };
            match job() {
                Ok(p) => res.written.push(p.to_string_lossy().into()),
                Err(e) => res.failed.push(format!("#{id}: {e}")),
            }
        }
        Ok(res)
    })
    .await
    .map_err(s)?
}

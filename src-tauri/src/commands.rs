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
use prism_core::geocode::{cache_key, format_coords};
use prism_core::metadata::read_exif;
use prism_core::overlay::{caption_lines, draw_caption, Caption};
use prism_core::{xmp, Asset, Catalog, DevelopSettings, SearchQuery};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager, State};

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
    /// `[lat, lon]`, read live from the file.
    gps: Option<[f64; 2]>,
}

#[tauri::command]
pub fn asset_details(state: State<'_, AppState>, id: i64) -> CmdResult<AssetDetails> {
    let (asset, keywords) = {
        let cat = state.catalog();
        (cat.asset(id).map_err(s)?, cat.keywords_of(id).map_err(s)?)
    };
    let gps = read_exif(Path::new(&asset.path)).gps;
    Ok(AssetDetails { asset, keywords, gps })
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

/// Why a caption came out different from what was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptionIssue {
    NoGps,
    LookupFailed,
}

/// Place line for a caption: catalog cache → OpenStreetMap → coordinates.
fn resolve_place(state: &AppState, gps: Option<[f64; 2]>, language: &str) -> (Option<String>, Option<CaptionIssue>) {
    let Some([lat, lon]) = gps else { return (None, Some(CaptionIssue::NoGps)) };
    let key = format!("{}|{language}", cache_key(lat, lon));
    if let Ok(Some(cached)) = state.catalog().place(&key) {
        return (Some(cached.unwrap_or_else(|| format_coords(lat, lon))), None);
    }
    match state.geocoder.reverse(lat, lon, language) {
        Ok(place) => {
            let _ = state.catalog().set_place(&key, place.as_deref());
            (Some(place.unwrap_or_else(|| format_coords(lat, lon))), None)
        }
        Err(_) => (Some(format_coords(lat, lon)), Some(CaptionIssue::LookupFailed)),
    }
}

/// Burns the caption into `img`. Returns what (if anything) was missing.
fn apply_caption(state: &AppState, asset: &Asset, img: &mut image::RgbImage, c: &Caption) -> Option<CaptionIssue> {
    let (place, issue) = if c.location {
        resolve_place(state, read_exif(Path::new(&asset.path)).gps, &c.place_language)
    } else {
        (None, None)
    };
    draw_caption(img, &caption_lines(c, asset.taken_at.as_deref(), place.as_deref()), c);
    issue
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionPreview {
    data_url: String,
    note: Option<String>,
}

/// The current photo with its develop settings and the caption, small.
#[tauri::command]
pub async fn caption_preview(app: AppHandle, id: i64, caption: Caption) -> CmdResult<CaptionPreview> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let (asset, settings) = {
            let cat = state.catalog();
            (cat.asset(id).map_err(s)?, cat.develop(id).map_err(s)?.unwrap_or_default())
        };
        let src = state.source(id, Path::new(&asset.path)).map_err(s)?;
        let mut img = render_to_rgb8(&src, &settings);
        if img.width().max(img.height()) > 900 {
            img = image::imageops::thumbnail(&img, 900, 900 * img.height() / img.width().max(1));
        }
        let note = match apply_caption(&state, &asset, &mut img, &caption) {
            Some(CaptionIssue::NoGps) => Some("This photo has no GPS location.".into()),
            Some(CaptionIssue::LookupFailed) => Some("Place lookup failed (offline?) — coordinates shown.".into()),
            None => None,
        };
        Ok(CaptionPreview {
            data_url: format!(
                "data:image/jpeg;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(encode_jpeg(&img, 85)?)
            ),
            note,
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
    /// Human-readable caveats (missing GPS, lookup failures).
    notes: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportProgress {
    done: usize,
    total: usize,
}

/// Renders each asset at full (or `max_dim`) resolution to `dir`,
/// optionally burning in a caption.
#[tauri::command]
pub async fn export(
    app: AppHandle,
    ids: Vec<i64>,
    dir: String,
    quality: u8,
    max_dim: u32,
    caption: Option<Caption>,
) -> CmdResult<ExportResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).map_err(s)?;
        let mut res = ExportResult { written: Vec::new(), failed: Vec::new(), notes: Vec::new() };
        let (mut no_gps, mut lookup_failed) = (0, 0);
        let total = ids.len();
        for (i, id) in ids.into_iter().enumerate() {
            let mut job = || -> Result<PathBuf, String> {
                let (asset, settings) = {
                    let cat = state.catalog();
                    (cat.asset(id).map_err(s)?, cat.develop(id).map_err(s)?.unwrap_or_default())
                };
                let src = prism_core::imaging::load_linear(Path::new(&asset.path), max_dim).map_err(s)?;
                let mut img = render_to_rgb8(&src, &settings);
                if let Some(c) = &caption {
                    match apply_caption(&state, &asset, &mut img, c) {
                        Some(CaptionIssue::NoGps) => no_gps += 1,
                        Some(CaptionIssue::LookupFailed) => lookup_failed += 1,
                        None => {}
                    }
                }
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
            let _ = app.emit("export-progress", ExportProgress { done: i + 1, total });
        }
        if no_gps > 0 {
            res.notes.push(format!("{no_gps} photo(s) have no GPS location, so no place was added"));
        }
        if lookup_failed > 0 {
            res.notes.push(format!("place lookup failed for {lookup_failed} photo(s); coordinates were used instead"));
        }
        Ok(res)
    })
    .await
    .map_err(s)?
}

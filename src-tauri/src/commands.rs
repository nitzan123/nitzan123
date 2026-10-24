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
use prism_core::embed::{encode_with_meta, ExportMeta};
use prism_core::geocode::PlaceDetail;
use prism_core::locate::{self, Estimate, Source, Stop};
use prism_core::metadata::date_from_filename;
use prism_core::metadata::read_exif;
use prism_core::overlay::{format_date, DateFormat, TextOverlay};
use prism_core::workflow::file_date;
use prism_core::workflow::{
    apply_preset, builtin_presets, execute_renames, look_of, plan_renames, unique_path, ExportFormat, NameTemplate,
    Preset, RenamePlan,
};
use prism_core::{scan, xmp, Asset, Catalog, DevelopSettings, SearchQuery};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    let state = app.state::<AppState>();
    state.catalog().add_root(&p.to_string_lossy(), recursive).map_err(s)?;
    app.state::<crate::watcher::Watch>().add(&p, recursive);
    indexer::spawn(app, p, recursive);
    Ok(())
}

/// Re-indexes one folder (its files only) without changing library roots.
#[tauri::command]
pub fn rescan_folder(app: AppHandle, path: String) -> CmdResult<()> {
    let p = PathBuf::from(&path);
    if !p.is_dir() {
        return Err(format!("not a folder: {path}"));
    }
    indexer::spawn(app, p, false);
    Ok(())
}

/// Takes a folder (and everything below it) out of the library. Files on
/// disk are never touched; ratings and edits survive in XMP sidecars, so
/// adding the folder again brings them back.
#[tauri::command]
pub fn remove_folder(app: AppHandle, path: String) -> CmdResult<prism_core::catalog::RemovedFolder> {
    let removed = app.state::<AppState>().catalog().remove_folder(&path).map_err(s)?;
    let watch = app.state::<crate::watcher::Watch>();
    for root in &removed.unwatched {
        watch.remove(Path::new(root));
    }
    Ok(removed)
}

#[tauri::command]
pub fn folders(state: State<'_, AppState>) -> CmdResult<Vec<FolderInfo>> {
    state.catalog().folders().map_err(s)
}

#[tauri::command]
pub fn calendar(state: State<'_, AppState>) -> CmdResult<prism_core::catalog::Calendar> {
    state.catalog().calendar().map_err(s)
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

/// Visually similar photos: CLIP embeddings when the library has been
/// analysed (finds similar scenes, not just copies), else perceptual hash.
#[tauri::command]
pub fn similar(state: State<'_, AppState>, id: i64, max_distance: Option<u32>) -> CmdResult<Vec<SimilarHit>> {
    let cat = state.catalog();
    let embeddings = cat.embeddings().map_err(s)?;
    if let Some((_, target)) = embeddings.iter().find(|(i, _)| *i == id) {
        let mut hits: Vec<(i64, f32)> = embeddings
            .iter()
            .filter(|(i, _)| *i != id)
            .map(|(i, e)| (*i, prism_core::ai::cosine(target, e)))
            .filter(|(_, sim)| *sim >= 0.85)
            .collect();
        hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        return hits
            .into_iter()
            .take(24)
            .map(|(i, sim)| {
                Ok(SimilarHit { asset: cat.asset(i).map_err(s)?, distance: ((1.0 - sim) * 100.0).round() as u32 })
            })
            .collect();
    }
    Ok(cat
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

// --------------------------------------------------------- text on photos

/// How to fill in the text of the photos in a batch.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TextApply {
    /// Position, size, colour and font for every photo in the batch.
    style: TextOverlay,
    date: bool,
    date_format: DateFormat,
    place: bool,
    place_language: String,
    place_detail: PlaceDetail,
    /// Use the file's modified date when a photo has no capture date.
    use_file_date: bool,
}

impl Default for TextApply {
    fn default() -> Self {
        Self {
            style: TextOverlay::default(),
            date: true,
            date_format: DateFormat::Dotted,
            place: true,
            place_language: String::new(),
            place_detail: PlaceDetail::Venue,
            use_file_date: false,
        }
    }
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TextApplyResult {
    applied: usize,
    /// Places worked out from other photos or the location history.
    estimated: usize,
    /// Places named after the nearest venue on the map.
    venues: usize,
    /// Dates taken from file names or file dates.
    date_to_check: usize,
    /// e.g. detected camera clock offsets.
    notes: Vec<String>,
    /// Photos whose place is still empty (no GPS, or lookup failed).
    need_place: usize,
    no_date: usize,
    /// The place lookup failed for some photos (offline?).
    lookup_failed: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TextProgress {
    done: usize,
    total: usize,
}

/// Town for coordinates: catalog cache, then OpenStreetMap. `Err` when the
/// lookup failed (offline); `Ok(None)` when there is no town there.
fn town(state: &AppState, [lat, lon]: [f64; 2], language: &str, detail: PlaceDetail) -> Result<Option<String>, ()> {
    let key = format!("{}|{language}|{detail:?}", detail.cache_key(lat, lon));
    if let Ok(Some(cached)) = state.catalog().place(&key) {
        // Names cached before alternate-language parts were dropped.
        return Ok(cached.map(|p| prism_core::geocode::primary_name(&p).to_string()));
    }
    let place = state.geocoder.lookup(lat, lon, language, detail).map_err(|_| ())?;
    let _ = state.catalog().set_place(&key, place.as_deref());
    Ok(place)
}

/// Adds or refreshes the text (subject, place, date) on photos. Data from
/// the photo comes first (capture date, GPS → town); anything the user
/// typed before (subject, a corrected place) is kept. Photos left without
/// a place are flagged in the library so it can be typed in.
#[tauri::command]
pub async fn text_apply(app: AppHandle, ids: Vec<i64>, options: TextApply) -> CmdResult<TextApplyResult> {
    tauri::async_runtime::spawn_blocking(move || {
        // A Timeline.json copied in since the app started.
        auto_import_history(&app);
        let state = app.state::<AppState>();
        let mut res = TextApplyResult::default();
        let total = ids.len();
        let assets: Vec<Asset> = {
            let cat = state.catalog();
            ids.iter().map(|&id| cat.asset(id)).collect::<prism_core::Result<_>>().map_err(s)?
        };
        // Where photos without GPS were probably taken, from photos with GPS
        // (same camera or a phone) and the imported location history.
        let estimates: HashMap<i64, Estimate> = if options.place {
            // Every dated photo without GPS takes part (so camera clock
            // offsets are detected from all of a camera's photos); only the
            // selected ones are written.
            let (candidates, refs, track) = {
                let cat = state.catalog();
                (cat.gps_targets().map_err(s)?, cat.gps_refs().map_err(s)?, cat.track().map_err(s)?)
            };
            let selected: std::collections::HashSet<i64> = ids.iter().copied().collect();
            let (found, notes) = locate::estimate(&candidates, &refs, &track);
            res.notes = notes;
            found.into_iter().filter(|e| selected.contains(&e.id)).map(|e| (e.id, e)).collect()
        } else {
            HashMap::new()
        };
        for (i, asset) in assets.iter().enumerate() {
            let id = asset.id;
            let mut settings = state.catalog().develop(id).map_err(s)?.unwrap_or_default();
            let old = settings.text.take().unwrap_or_default();
            let typed = |from: Source, value: &str| {
                // Text from before sources were recorded counts as typed.
                !value.trim().is_empty() && matches!(from, Source::Typed | Source::None)
            };
            let mut t = TextOverlay { subject: old.subject.clone(), ..options.style.clone() };

            // Date: typed > camera > file name > file date.
            (t.date, t.date_from) = if !options.date {
                (String::new(), Source::None)
            } else if typed(old.date_from, &old.date) {
                (old.date.clone(), Source::Typed)
            } else {
                let fmt = |d: &str| format_date(d, options.date_format);
                let from_name = date_from_filename(&asset.file_name);
                match asset.taken_at.as_deref() {
                    Some(d) if from_name.as_deref() == Some(d) => (fmt(d).unwrap_or_default(), Source::FileName),
                    Some(d) => (fmt(d).unwrap_or_default(), Source::Photo),
                    None if options.use_file_date => {
                        (fmt(&file_date(asset.mtime)).unwrap_or_default(), Source::FileDate)
                    }
                    None => (String::new(), Source::None),
                }
            };

            // Place: typed > GPS > estimate.
            let mut lookup =
                |gps: [f64; 2], detail: PlaceDetail| match town(&state, gps, &options.place_language, detail) {
                    Ok(p) => p.unwrap_or_default(),
                    Err(()) => {
                        res.lookup_failed = true;
                        String::new()
                    }
                };
            // An estimated position gets a venue only when it is a stop in
            // the location history (the phone stayed at that place);
            // elsewhere it would be a guess on a guess.
            let estimate_detail =
                if options.place_detail == PlaceDetail::Venue { PlaceDetail::City } else { options.place_detail };
            let venue_mode = options.place_detail == PlaceDetail::Venue;
            (t.place, t.place_from, t.place_note) = if !options.place {
                (String::new(), Source::None, String::new())
            } else if typed(old.place_from, &old.place) {
                (old.place.clone(), Source::Typed, String::new())
            } else if let Some(g) = asset.gps {
                let place = lookup(g, options.place_detail);
                if options.place_detail == PlaceDetail::Venue
                    && !place.is_empty()
                    && place != lookup(g, PlaceDetail::City)
                {
                    res.venues += 1;
                    (place, Source::Venue, String::new())
                } else {
                    (place, Source::Photo, String::new())
                }
            } else if let Some(e) = estimates
                .get(&id)
                .map(|e| {
                    let place = match (&e.stop, venue_mode) {
                        // The export named the place: no map lookup needed.
                        (Some(Stop { name: Some(n) }), true) => prism_core::geocode::join_venue(
                            Some(n.clone()),
                            Some(lookup([e.lat, e.lon], PlaceDetail::City)).filter(|t| !t.is_empty()),
                        )
                        .unwrap_or_default(),
                        (Some(_), true) => lookup([e.lat, e.lon], PlaceDetail::Venue),
                        _ => lookup([e.lat, e.lon], estimate_detail),
                    };
                    (e, place)
                })
                .filter(|(_, p)| !p.is_empty())
            {
                let (e, place) = e;
                // Only a same-camera neighbour minutes apart, with the photos
                // around it agreeing, is taken as is; the rest is flagged.
                let from = if e.confident && e.source == Source::Nearby { Source::Typed } else { e.source };
                res.estimated += from.is_estimate() as usize;
                (place, from, e.note.clone())
            } else if !old.place.trim().is_empty() && old.place_from.is_estimate() {
                // Nothing new (or offline): keep the earlier estimate.
                res.estimated += 1;
                (old.place.clone(), old.place_from, old.place_note.clone())
            } else {
                (String::new(), Source::None, String::new())
            };
            res.need_place += (options.place && t.place.is_empty()) as usize;
            res.no_date += (options.date && t.date.is_empty()) as usize;
            res.date_to_check += (t.date_from.is_estimate() && !t.date.is_empty()) as usize;
            settings.text = Some(t);
            {
                let cat = state.catalog();
                cat.set_develop(id, Some(&settings)).map_err(s)?;
                sync_sidecars(&cat, &[id]);
            }
            res.applied += 1;
            let _ = app.emit("text-progress", TextProgress { done: i + 1, total });
        }
        Ok(res)
    })
    .await
    .map_err(s)?
}

/// Accepts the inferred places and dates of these photos as correct.
#[tauri::command]
pub fn text_confirm(state: State<'_, AppState>, ids: Vec<i64>) -> CmdResult<()> {
    let cat = state.catalog();
    for &id in &ids {
        if let Some(mut settings) = cat.develop(id).map_err(s)? {
            if let Some(t) = settings.text.as_mut() {
                t.confirm();
                cat.set_develop(id, Some(&settings)).map_err(s)?;
            }
        }
    }
    sync_sidecars(&cat, &ids);
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackSummary {
    points: i64,
    /// `YYYY-MM-DD` of the first and last point.
    from: Option<String>,
    to: Option<String>,
}

fn track_summary(cat: &Catalog) -> CmdResult<TrackSummary> {
    let (points, a, b) = cat.track_summary().map_err(s)?;
    let day = |t: Option<i64>| t.map(|t| file_date(t)[..10].to_string());
    Ok(TrackSummary { points, from: day(a), to: day(b) })
}

#[tauri::command]
pub fn location_history(state: State<'_, AppState>) -> CmdResult<TrackSummary> {
    track_summary(&state.catalog())
}

/// Replaces the location history with the file at `path` and remembers
/// which file (and version of it) that was.
fn import_track_file(state: &AppState, path: &Path) -> Result<TrackSummary, String> {
    let mtime = scan::stat(path).map(|(_, m)| m).unwrap_or(0);
    let file = std::fs::File::open(path).map_err(s)?;
    let json: serde_json::Value = serde_json::from_reader(std::io::BufReader::new(file))
        .map_err(|e| format!("not a location-history file: {e}"))?;
    let points = locate::parse_timeline(&json);
    if points.is_empty() {
        return Err(
            "No locations found in this file. Export it from Google Maps → Timeline (see the help text).".into()
        );
    }
    let mut cat = state.catalog();
    cat.set_track(&points).map_err(s)?;
    let _ = cat.set_setting("history_file", &format!("{mtime}|{}", path.display()));
    track_summary(&cat)
}

/// Loads a location-history export the user put in Downloads, Desktop,
/// Documents or a library folder, so it needn't be imported by hand. Only
/// a file newer than the one loaded last is taken. Returns what was loaded.
pub fn auto_import_history(app: &AppHandle) -> Option<(String, TrackSummary)> {
    let state = app.state::<AppState>();
    let paths = app.path();
    let mut dirs: Vec<PathBuf> =
        [paths.download_dir(), paths.desktop_dir(), paths.document_dir()].into_iter().flatten().collect();
    // Where the system doesn't name these folders (some Linux setups).
    if let Ok(home) = paths.home_dir() {
        dirs.extend(["Downloads", "Desktop", "Documents"].map(|d| home.join(d)));
    }
    dirs.sort();
    dirs.dedup();
    dirs.extend(state.catalog().roots().unwrap_or_default().into_iter().map(|(r, _)| PathBuf::from(r)));
    let (path, mtime) = locate::find_history_file(&dirs)?;
    let loaded: i64 = state
        .catalog()
        .setting("history_file")
        .ok()
        .flatten()
        .and_then(|v| v.split('|').next().and_then(|m| m.parse().ok()))
        .unwrap_or(i64::MIN);
    if mtime <= loaded {
        return None;
    }
    let summary = import_track_file(&state, &path).ok()?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let _ = app.emit("history-imported", (&name, &summary));
    Some((name, summary))
}

/// Imports a Google Maps location-history export (replacing any earlier
/// import). Only the time and position of each point are kept, plus
/// whether the phone was staying at a place then (and its name, if given).
#[tauri::command]
pub async fn import_location_history(app: AppHandle, path: String) -> CmdResult<TrackSummary> {
    tauri::async_runtime::spawn_blocking(move || import_track_file(&app.state::<AppState>(), Path::new(&path)))
        .await
        .map_err(s)?
}

/// Sets the subject and/or place on several photos at once (fields left
/// `None` are untouched). Photos without text get it with default style.
#[tauri::command]
pub fn text_edit(
    state: State<'_, AppState>,
    ids: Vec<i64>,
    subject: Option<String>,
    place: Option<String>,
) -> CmdResult<()> {
    let cat = state.catalog();
    for &id in &ids {
        let mut settings = cat.develop(id).map_err(s)?.unwrap_or_default();
        let t = settings.text.get_or_insert_with(TextOverlay::default);
        if let Some(v) = &subject {
            t.subject = v.trim().to_string();
        }
        if let Some(v) = &place {
            t.place = v.trim().to_string();
            t.place_from = Source::Typed;
            t.place_note.clear();
        }
        cat.set_develop(id, Some(&settings)).map_err(s)?;
    }
    sync_sidecars(&cat, &ids);
    Ok(())
}

#[tauri::command]
pub fn text_remove(state: State<'_, AppState>, ids: Vec<i64>) -> CmdResult<()> {
    let cat = state.catalog();
    for &id in &ids {
        if let Some(mut settings) = cat.develop(id).map_err(s)? {
            settings.text = None;
            cat.set_develop(id, Some(&settings)).map_err(s)?;
        }
    }
    sync_sidecars(&cat, &ids);
    Ok(())
}

/// Renames a place on every photo that has it ("Lince" → "Lima").
#[tauri::command]
pub fn replace_place(state: State<'_, AppState>, old: String, new: String) -> CmdResult<Vec<i64>> {
    let cat = state.catalog();
    let ids = cat.replace_place(&old, &new).map_err(s)?;
    sync_sidecars(&cat, &ids);
    Ok(ids)
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExportOptions {
    format: ExportFormat,
    /// JPEG quality 10…100.
    quality: u8,
    /// Long edge in pixels; 0 = full size.
    max_dim: u32,
    /// File-name template (see `workflow::NameTemplate`); empty = original names.
    name_template: Option<String>,
    /// Embed EXIF/XMP (camera, date, keywords, caption, rating).
    metadata: bool,
    /// Keep the GPS position in embedded metadata.
    include_gps: bool,
    /// Copyright notice written to EXIF and XMP.
    copyright: Option<String>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: ExportFormat::Jpeg,
            quality: 92,
            max_dim: 0,
            name_template: None,
            metadata: true,
            include_gps: true,
            copyright: None,
        }
    }
}

/// Renders each asset at full (or `max_dim`) resolution to `dir`, with its
/// develop settings (including any text on the photo).
#[tauri::command]
pub async fn export(app: AppHandle, ids: Vec<i64>, dir: String, options: ExportOptions) -> CmdResult<ExportResult> {
    let ExportOptions { format, quality, max_dim, name_template, metadata, include_gps, copyright } = options;
    let template = match name_template.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => Some(NameTemplate::parse(t).map_err(s)?),
        None => None,
    };
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).map_err(s)?;
        let mut res = ExportResult { written: Vec::new(), failed: Vec::new(), notes: Vec::new() };
        let mut no_place = 0;
        let total = ids.len();
        for (i, id) in ids.into_iter().enumerate() {
            let mut job = || -> Result<PathBuf, String> {
                let (asset, settings, keywords) = {
                    let cat = state.catalog();
                    let kw = if metadata { cat.keywords_of(id).map_err(s)? } else { Vec::new() };
                    (cat.asset(id).map_err(s)?, cat.develop(id).map_err(s)?.unwrap_or_default(), kw)
                };
                let src = prism_core::imaging::load_linear(Path::new(&asset.path), max_dim).map_err(s)?;
                let img = render_to_rgb8(&src, &settings);
                no_place += asset.needs_place as usize;
                let stem = match &template {
                    Some(t) => t.render(&asset, i + 1),
                    None => Path::new(&asset.file_name).file_stem().unwrap_or_default().to_string_lossy().to_string(),
                };
                let target = unique_path(&dir, &stem, format.extension());
                let bytes = if metadata {
                    let meta = ExportMeta::from_asset(&asset, keywords, include_gps, copyright.clone());
                    encode_with_meta(format, &img, quality, &meta).map_err(s)?
                } else {
                    format.encode(&img, quality).map_err(s)?
                };
                std::fs::write(&target, bytes).map_err(s)?;
                Ok(target)
            };
            match job() {
                Ok(p) => res.written.push(p.to_string_lossy().into()),
                Err(e) => res.failed.push(format!("#{id}: {e}")),
            }
            let _ = app.emit("export-progress", ExportProgress { done: i + 1, total });
        }
        if no_place > 0 {
            res.notes.push(format!("{no_place} photo(s) were exported with text but without a place"));
        }
        Ok(res)
    })
    .await
    .map_err(s)?
}

// ----------------------------------------------------------------- presets

/// Built-in presets first, then the user's.
#[tauri::command]
pub fn presets(state: State<'_, AppState>) -> CmdResult<Vec<Preset>> {
    let mut out = builtin_presets();
    out.extend(state.catalog().presets().map_err(s)?);
    Ok(out)
}

/// Saves the look of `settings` (not crop, lens or masks) as a preset.
#[tauri::command]
pub fn save_preset(state: State<'_, AppState>, name: String, settings: DevelopSettings) -> CmdResult<i64> {
    state.catalog().save_preset(&name, &look_of(&settings)).map_err(s)
}

#[tauri::command]
pub fn delete_preset(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    state.catalog().delete_preset(id).map_err(s)
}

/// Applies a preset's look to every photo in `ids`, keeping each photo's
/// crop, lens correction and local adjustments.
#[tauri::command]
pub fn apply_preset_to(state: State<'_, AppState>, ids: Vec<i64>, settings: DevelopSettings) -> CmdResult<()> {
    let cat = state.catalog();
    for &id in &ids {
        let base = cat.develop(id).map_err(s)?.unwrap_or_default();
        cat.set_develop(id, Some(&apply_preset(&base, &settings))).map_err(s)?;
    }
    sync_sidecars(&cat, &ids);
    Ok(())
}

// ------------------------------------------------------------ batch rename

fn rename_plans(state: &AppState, ids: &[i64], template: &str, start: usize) -> CmdResult<Vec<RenamePlan>> {
    let t = NameTemplate::parse(template).map_err(s)?;
    let assets: Vec<Asset> = {
        let cat = state.catalog();
        ids.iter().map(|&id| cat.asset(id)).collect::<prism_core::Result<_>>().map_err(s)?
    };
    let mut plans = plan_renames(&assets, &t, start);
    if !t.has_unique_part() && plans.len() > 1 {
        for p in plans.iter_mut().skip(1) {
            p.problem.get_or_insert_with(|| "add {name} or {seq} so names are unique".into());
        }
    }
    Ok(plans)
}

#[tauri::command]
pub fn rename_preview(
    state: State<'_, AppState>,
    ids: Vec<i64>,
    template: String,
    start: usize,
) -> CmdResult<Vec<RenamePlan>> {
    rename_plans(&state, &ids, &template, start)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameResult {
    renamed: usize,
    failed: Vec<String>,
}

/// Renames files on disk (with their sidecars) and updates the catalog.
/// Only plans without problems run; the rest are reported.
#[tauri::command]
pub async fn rename_apply(app: AppHandle, ids: Vec<i64>, template: String, start: usize) -> CmdResult<RenameResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let plans = rename_plans(&state, &ids, &template, start)?;
        let mut failed: Vec<String> =
            plans.iter().filter_map(|p| p.problem.as_ref().map(|e| format!("{}: {e}", file_name(&p.from)))).collect();
        let (ok, errs) = execute_renames(&plans);
        let cat = state.catalog();
        for (id, path) in &ok {
            if let Err(e) = cat.rename_asset(*id, path) {
                failed.push(format!("{path}: {e}"));
            }
        }
        failed.extend(errs.iter().map(|(id, e)| format!("#{id}: {e}")));
        Ok(RenameResult { renamed: ok.len(), failed })
    })
    .await
    .map_err(s)?
}

fn file_name(p: &str) -> String {
    Path::new(p).file_name().map(|n| n.to_string_lossy().into()).unwrap_or_else(|| p.into())
}

// ------------------------------------------------------------------- lenses

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LensDbStatus {
    installed: bool,
    lenses: usize,
}

#[tauri::command]
pub async fn lens_db_status(app: AppHandle) -> CmdResult<LensDbStatus> {
    tauri::async_runtime::spawn_blocking(move || {
        let db = app.state::<AppState>().lenses.get();
        Ok(LensDbStatus { installed: db.is_some(), lenses: db.map_or(0, |d| d.lenses.len()) })
    })
    .await
    .map_err(s)?
}

/// Downloads the lensfun database (~5 MB). Emits `lens-db-progress`.
#[tauri::command]
pub async fn lens_db_download(app: AppHandle) -> CmdResult<LensDbStatus> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let db = prism_core::lens::download_db(&state.lenses.dir, |done, total| {
            let _ = app.emit("lens-db-progress", [done, total]);
        })
        .map_err(s)?;
        let n = db.lenses.len();
        *state.lenses.db.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::sync::Arc::new(db));
        Ok(LensDbStatus { installed: true, lenses: n })
    })
    .await
    .map_err(s)?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LensMatch {
    /// What the photo's EXIF says.
    exif_lens: Option<String>,
    camera: Option<String>,
    profile: Option<prism_core::lens::LensProfile>,
    message: String,
}

/// Resolves the lens profile for a photo from the database.
#[tauri::command]
pub async fn lens_profile(app: AppHandle, id: i64) -> CmdResult<LensMatch> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let asset = state.catalog().asset(id).map_err(s)?;
        let e = read_exif(Path::new(&asset.path));
        let camera =
            [e.camera_make.clone(), e.camera_model.clone()].into_iter().flatten().collect::<Vec<_>>().join(" ");
        let camera = Some(camera).filter(|c| !c.is_empty());
        let Some(db) = state.lenses.get() else {
            return Ok(LensMatch {
                exif_lens: e.lens,
                camera,
                profile: None,
                message: "Lens database not installed".into(),
            });
        };
        let Some(lens_name) = e.lens.clone() else {
            return Ok(LensMatch {
                exif_lens: None,
                camera,
                profile: None,
                message: "The photo doesn't record which lens was used".into(),
            });
        };
        let crop = db
            .camera_crop(e.camera_make.as_deref().unwrap_or(""), e.camera_model.as_deref().unwrap_or(""))
            .unwrap_or(1.0);
        let Some(entry) = db.find_lens(&lens_name, crop) else {
            return Ok(LensMatch {
                exif_lens: Some(lens_name.clone()),
                camera,
                profile: None,
                message: format!("No profile for \"{lens_name}\""),
            });
        };
        let focal = e.focal_length.unwrap_or(0.0);
        let profile = entry.profile(focal, e.f_number.unwrap_or(0.0), crop);
        let message = format!("{} @ {:.0} mm", profile.lens, focal);
        Ok(LensMatch { exif_lens: Some(lens_name), camera, profile: Some(profile), message })
    })
    .await
    .map_err(s)?
}

// ----------------------------------------------------------------------- AI

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiStatus {
    installed: bool,
    download_bytes: u64,
    running: bool,
    counts: prism_core::catalog::AiCounts,
}

#[tauri::command]
pub fn ai_status(state: State<'_, AppState>) -> CmdResult<AiStatus> {
    Ok(AiStatus {
        installed: state.ai.installed(),
        download_bytes: prism_core::ai::total_download_size(),
        running: state.ai_running.load(std::sync::atomic::Ordering::SeqCst),
        counts: state.catalog().ai_counts().map_err(s)?,
    })
}

/// Downloads the models (~130 MB). Emits `ai-download-progress` [done, total].
#[tauri::command]
pub async fn ai_download(app: AppHandle) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = app.state::<AppState>().ai.dir().to_path_buf();
        prism_core::ai::download(&dir, |d, t| {
            let _ = app.emit("ai-download-progress", [d, t]);
        })
        .map_err(s)
    })
    .await
    .map_err(s)?
}

#[tauri::command]
pub fn ai_analyze(app: AppHandle) -> CmdResult<()> {
    crate::ai_job::spawn(app)
}

#[tauri::command]
pub fn ai_cancel(state: State<'_, AppState>) {
    state.ai_cancel.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiDetails {
    tags: Vec<(String, f32)>,
    faces: Vec<prism_core::catalog::FaceRow>,
}

#[tauri::command]
pub fn ai_details(state: State<'_, AppState>, id: i64) -> CmdResult<AiDetails> {
    let cat = state.catalog();
    Ok(AiDetails { tags: cat.ai_tags(id).map_err(s)?, faces: cat.faces_of(id).map_err(s)? })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoredAsset {
    asset: Asset,
    score: f32,
}

/// Natural-language search over CLIP embeddings ("dog on the beach").
#[tauri::command]
pub async fn smart_search(app: AppHandle, text: String, limit: Option<usize>) -> CmdResult<Vec<ScoredAsset>> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let q = state.ai.clip().map_err(s)?.embed_text(&text).map_err(s)?;
        let embeddings = state.catalog().embeddings().map_err(s)?;
        let mut scored: Vec<(i64, f32)> =
            embeddings.iter().map(|(id, e)| (*id, prism_core::ai::cosine(&q, e))).collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        // CLIP scores are relative: keep the clear matches near the top.
        let best = scored.first().map_or(0.0, |x| x.1);
        let cat = state.catalog();
        scored
            .into_iter()
            .take_while(|(_, sc)| *sc >= best - 0.06 && *sc > 0.15)
            .take(limit.unwrap_or(200))
            .map(|(id, score)| Ok(ScoredAsset { asset: cat.asset(id).map_err(s)?, score }))
            .collect()
    })
    .await
    .map_err(s)?
}

#[tauri::command]
pub fn persons(state: State<'_, AppState>) -> CmdResult<Vec<prism_core::catalog::PersonRow>> {
    state.catalog().persons().map_err(s)
}

#[tauri::command]
pub fn name_person(state: State<'_, AppState>, person_id: i64, name: String) -> CmdResult<i64> {
    state.catalog().name_person(person_id, &name).map_err(s)
}

#[tauri::command]
pub fn set_face_name(state: State<'_, AppState>, face_id: i64, name: String) -> CmdResult<()> {
    state.catalog().set_face_name(face_id, &name).map_err(s)
}

/// AI mask ("subject" or "sky") for a photo, ready to put in a local adjustment.
#[tauri::command]
pub async fn ai_mask(app: AppHandle, id: i64, kind: String) -> CmdResult<prism_core::develop::masks::Mask> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let path = state.catalog().asset(id).map_err(s)?.path;
        let img = prism_core::imaging::load_display(Path::new(&path), 1024).map_err(s)?;
        prism_core::ai::mask(&state.ai, &img, &kind).map_err(s)
    })
    .await
    .map_err(s)?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MappedPoints {
    /// Source width / height.
    aspect: f32,
    points: Vec<Option<[f32; 2]>>,
}

/// Maps normalised points between the source photo and the developed frame
/// (see `pipeline::map_points`). Used to place retouch spots.
#[tauri::command]
pub async fn map_points(
    app: AppHandle,
    id: i64,
    settings: DevelopSettings,
    points: Vec<[f32; 2]>,
    to_source: bool,
) -> CmdResult<MappedPoints> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let path = state.catalog().asset(id).map_err(s)?.path;
        let src = state.source(id, Path::new(&path)).map_err(s)?;
        let aspect = src.width() as f32 / src.height().max(1) as f32;
        let points = prism_core::develop::pipeline::map_points(aspect, &settings, &points, to_source);
        Ok(MappedPoints { aspect, points })
    })
    .await
    .map_err(s)?
}

/// Best place to copy from for a new retouch spot (source coordinates),
/// judged on the photo with the existing spots applied.
#[tauri::command]
pub async fn spot_source(
    app: AppHandle,
    id: i64,
    settings: DevelopSettings,
    x: f32,
    y: f32,
    radius: f32,
) -> CmdResult<[f32; 2]> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let path = state.catalog().asset(id).map_err(s)?.path;
        let src = state.source(id, Path::new(&path)).map_err(s)?;
        Ok(prism_core::develop::retouch::auto_source_in(&src, &settings.spots, x, y, radius))
    })
    .await
    .map_err(s)?
}

/// Scales the whole interface, like browser zoom (Ctrl + / Ctrl − / Ctrl 0).
#[tauri::command]
pub fn set_zoom(window: tauri::WebviewWindow, scale: f64) -> CmdResult<()> {
    window.set_zoom(scale.clamp(0.5, 2.0)).map_err(s)
}

/// A photo the OS asked us to open before the UI was listening (once).
#[tauri::command]
pub fn take_pending_open(pending: State<'_, crate::open::PendingOpen>) -> Option<String> {
    pending.0.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// The catalog entry for a file path, if it has been indexed.
#[tauri::command]
pub fn asset_by_path(state: State<'_, AppState>, path: String) -> CmdResult<Option<Asset>> {
    state.catalog().asset_by_path(&path).map_err(s)
}

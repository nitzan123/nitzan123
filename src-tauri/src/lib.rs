//! Tauri shell: owns app state and exposes `prism-core` to the web UI via
//! commands plus a `prism://` image protocol (thumbnails, previews).

mod ai_job;
mod commands;
mod indexer;
mod protocol;
mod state;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            let cache = app.path().app_cache_dir()?;
            app.manage(AppState::open(&data, &cache)?);
            indexer::spawn_gps_backfill(app.handle().clone());
            // `prism-studio <folder>…` indexes folders on start.
            for arg in std::env::args().skip(1) {
                let p = std::path::PathBuf::from(arg);
                if p.is_dir() {
                    indexer::spawn(app.handle().clone(), p, true);
                }
            }
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol("prism", protocol::handle)
        .invoke_handler(tauri::generate_handler![
            commands::add_folder,
            commands::rescan_folder,
            commands::folders,
            commands::calendar,
            commands::presets,
            commands::save_preset,
            commands::delete_preset,
            commands::apply_preset_to,
            commands::rename_preview,
            commands::rename_apply,
            commands::search,
            commands::asset_details,
            commands::set_rating,
            commands::set_color_label,
            commands::set_tagged,
            commands::set_caption,
            commands::add_keyword,
            commands::remove_keyword,
            commands::keywords,
            commands::collections,
            commands::create_collection,
            commands::add_to_collection,
            commands::remove_from_collection,
            commands::saved_searches,
            commands::save_search,
            commands::delete_saved_search,
            commands::similar,
            commands::develop_get,
            commands::develop_save,
            commands::develop_auto,
            commands::develop_auto_wb,
            commands::develop_preview,
            commands::export,
            commands::caption_preview,
            commands::lens_db_status,
            commands::lens_db_download,
            commands::lens_profile,
            commands::ai_status,
            commands::ai_download,
            commands::ai_analyze,
            commands::ai_cancel,
            commands::ai_details,
            commands::smart_search,
            commands::persons,
            commands::name_person,
            commands::set_face_name,
            commands::ai_mask,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Prism Studio");
}

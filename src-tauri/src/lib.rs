//! Tauri shell: owns app state and exposes `prism-core` to the web UI via
//! commands plus a `prism://` image protocol (thumbnails, previews).

mod ai_job;
mod commands;
mod indexer;
mod open;
mod protocol;
mod state;
mod watcher;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must come first: a second launch hands its arguments (e.g. a photo
        // double-clicked in the file manager) to this window and exits.
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            open::handle(app, open::from_args(argv.get(1..).unwrap_or_default(), std::path::Path::new(&cwd)), false);
        }))
        .plugin(tauri_plugin_dialog::init())
        .manage(open::PendingOpen::default())
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            let cache = app.path().app_cache_dir()?;
            app.manage(AppState::open(&data, &cache)?);
            indexer::spawn_gps_backfill(app.handle().clone());
            app.manage(watcher::Watch::start(app.handle()));
            // `prism-studio <folder | photo>…`: folders join the library; a
            // photo is shown once its folder is indexed.
            let args: Vec<String> = std::env::args().skip(1).collect();
            let cwd = std::env::current_dir().unwrap_or_default();
            open::handle(app.handle(), open::from_args(&args, &cwd), true);
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol("prism", protocol::handle)
        .invoke_handler(tauri::generate_handler![
            commands::add_folder,
            commands::rescan_folder,
            commands::remove_folder,
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
            commands::text_apply,
            commands::text_edit,
            commands::text_remove,
            commands::text_confirm,
            commands::location_history,
            commands::import_location_history,
            commands::replace_place,
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
            commands::map_points,
            commands::spot_source,
            commands::take_pending_open,
            commands::asset_by_path,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Prism Studio")
        .run(|_app, _event| {
            // macOS delivers "Open With" / double-clicked files as events.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = _event {
                let paths = urls.into_iter().filter_map(|u| u.to_file_path().ok());
                open::handle(_app, paths, false);
            }
        });
}

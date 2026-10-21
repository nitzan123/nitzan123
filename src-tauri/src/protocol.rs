//! `prism://localhost/thumb/<id>` — cached thumbnail (JPEG)
//! `prism://localhost/view/<id>?max=<px>` — developed full view (JPEG)
//!
//! Served off the UI thread; responses are cacheable by the webview because
//! the UI appends a version query when an asset changes.

use crate::state::AppState;
use prism_core::develop::render_to_rgb8;
use prism_core::imaging::load_linear;
use prism_core::thumbs::THUMB_SIZE;
use prism_core::DevelopSettings;
use std::path::Path;
use tauri::http::{Request, Response};
use tauri::{Manager, Runtime, UriSchemeContext, UriSchemeResponder};

pub fn handle<R: Runtime>(ctx: UriSchemeContext<'_, R>, req: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let app = ctx.app_handle().clone();
    // Bounded pool: scrolling a big uncached grid fires hundreds of requests;
    // unbounded threads would each decode a full image at once.
    pool().spawn(move || {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve(&app.state::<AppState>(), req.uri())))
            .unwrap_or_else(|_| Err("internal error while rendering".into()));
        responder.respond(match res {
            Ok(bytes) => Response::builder()
                .header("Content-Type", "image/jpeg")
                .header("Cache-Control", "max-age=31536000")
                .header("Access-Control-Allow-Origin", "*")
                .body(bytes)
                .unwrap(),
            Err(msg) => Response::builder().status(404).body(msg.into_bytes()).unwrap(),
        });
    });
}

fn pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 6));
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .thread_name(|i| format!("prism-img-{i}"))
            .build()
            .expect("image thread pool")
    })
}

fn serve(state: &AppState, uri: &tauri::http::Uri) -> Result<Vec<u8>, String> {
    let mut parts = uri.path().trim_start_matches('/').split('/');
    let kind = parts.next().unwrap_or_default();
    let id: i64 = parts.next().and_then(|s| s.parse().ok()).ok_or("bad id")?;
    let max: u32 = uri
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("max=")))
        .and_then(|v| v.parse().ok())
        .unwrap_or(2560)
        .clamp(256, 8192);

    let (asset, develop) = {
        let cat = state.catalog();
        let a = cat.asset(id).map_err(|e| e.to_string())?;
        let d = cat.develop(id).map_err(|e| e.to_string())?;
        (a, d)
    };
    let path = Path::new(&asset.path);
    match kind {
        "thumb" => {
            let file = match develop {
                None => state.thumbs.ensure(path, asset.mtime, asset.size).map_err(|e| e.to_string())?.0,
                // Developed photos get a rendered thumbnail, cached per settings.
                Some(s) => {
                    let variant = serde_json::to_string(&s).map_err(|e| e.to_string())?;
                    state
                        .thumbs
                        .ensure_variant(path, asset.mtime, asset.size, &variant, || {
                            Ok(render_to_rgb8(&load_linear(path, THUMB_SIZE * 2)?, &s))
                        })
                        .map_err(|e| e.to_string())?
                }
            };
            std::fs::read(file).map_err(|e| e.to_string())
        }
        "view" => {
            let img = match develop {
                // Unedited: fast path through the embedded preview / decoder.
                None => prism_core::imaging::load_display(path, max).map_err(|e| e.to_string())?,
                Some(s) => render_view(path, max, &s)?,
            };
            crate::commands::encode_jpeg(&img, 88)
        }
        _ => Err("unknown route".into()),
    }
}

fn render_view(path: &Path, max: u32, s: &DevelopSettings) -> Result<image::RgbImage, String> {
    let src = load_linear(path, max).map_err(|e| e.to_string())?;
    Ok(render_to_rgb8(&src, s))
}

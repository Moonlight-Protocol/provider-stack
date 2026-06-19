//! Embedded SPA. With the `embed-spa` feature (on by default) the files under
//! `frontend/public/` are baked in at compile time via `include_dir!`, and
//! unknown routes fall back to `index.html` (SPA mode). Without the feature
//! (the CI `clippy`/`test` builds, which don't produce the frontend artifact)
//! the routes are still mounted but return 404 — no `frontend/public` is needed
//! to compile.

use crate::error::ApiError;
use actix_web::{web, HttpResponse};
#[cfg(feature = "embed-spa")]
use actix_web::http::header::CONTENT_TYPE;

#[cfg(feature = "embed-spa")]
static SPA_DIR: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../frontend/public");

#[cfg(feature = "embed-spa")]
pub async fn serve(req: actix_web::HttpRequest) -> Result<HttpResponse, ApiError> {
    let path = req.match_info().query("path").trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    let file = SPA_DIR
        .get_file(path)
        .or_else(|| SPA_DIR.get_file("index.html"))
        .ok_or(ApiError::NotFound)?;

    let mime = mime_for(path);
    Ok(HttpResponse::Ok()
        .insert_header((CONTENT_TYPE, mime))
        .body(file.contents()))
}

/// Fallback when the SPA is not embedded (e.g. CI `--no-default-features`
/// builds). The console is served by the canonical image; here there is nothing
/// baked in, so every SPA path is a 404.
#[cfg(not(feature = "embed-spa"))]
pub async fn serve(_req: actix_web::HttpRequest) -> Result<HttpResponse, ApiError> {
    Err(ApiError::NotFound)
}

#[cfg(feature = "embed-spa")]
fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" => "application/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/", web::get().to(serve))
        .route("/{path:.*}", web::get().to(serve));
}

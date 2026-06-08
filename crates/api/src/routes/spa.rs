//! Embedded SPA. Files baked in at compile time from `frontend/public/` via `include_dir!`.
//! Unknown routes fall back to `index.html` (SPA mode).

use crate::error::ApiError;
use actix_web::{
    http::header::{ContentType, CONTENT_TYPE},
    web, HttpResponse,
};
use include_dir::{include_dir, Dir};

static SPA_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../frontend/public");

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

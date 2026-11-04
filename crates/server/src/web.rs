//! The web UI, embedded into the binary.
//!
//! `web/dist` (the built Svelte app) is compiled into the executable, so a
//! release is a single file. In debug builds the files are read from disk, so
//! `npm run build` is enough to see UI changes without recompiling.
//!
//! Every path that is not an API route gets `index.html`, and the app's own
//! router shows the right page.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;
use rustcloud_core::Error;

use crate::plugins::Plugins;

#[derive(Embed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Assets;

/// What the browser may load on our pages: only our own scripts and styles;
/// in frames and as form targets also the services plugins may show
/// (`permissions.frame`, e.g. an office editor).
fn page_csp(services: &[String]) -> String {
    let services: String = services.iter().map(|origin| format!(" {origin}")).collect();
    format!(
        "default-src 'self'; img-src 'self' blob: data:; media-src 'self' blob:; \
         style-src 'self' 'unsafe-inline'; frame-src 'self'{services}; frame-ancestors 'none'; base-uri 'none'; \
         form-action 'self'{services}"
    )
}

/// Shown when the binary was built without the web UI.
const NO_UI_PAGE: &str = "<!doctype html><meta charset=utf-8><title>RustCloud</title>\
    <p>RustCloud is running, but this build has no web UI. \
    Build it with <code>npm run build</code> in <code>web/</code>.</p>";

/// Fallback handler: static files of the web UI, or `index.html` for app routes.
pub async fn serve(State(plugins): State<Arc<Plugins>>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path == "api" || path.starts_with("api/") {
        return Error::not_found("NOT_FOUND", "No such API endpoint").into_response();
    }

    if let Some(file) = Assets::get(path).filter(|_| !path.is_empty()) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        // Vite puts content hashes into file names under assets/, so they never change.
        let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
        return (
            [(header::CONTENT_TYPE, mime.essence_str().to_owned()), (header::CACHE_CONTROL, cache.to_owned())],
            Body::from(file.data),
        )
            .into_response();
    }

    let mut response = match Assets::get("index.html") {
        Some(index) => Body::from(index.data).into_response(),
        None => (StatusCode::OK, NO_UI_PAGE).into_response(),
    };
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    let services: Vec<String> = plugins.running_manifests().iter().flat_map(|m| plugins.frame_origins(&m.id)).collect();
    // Origins are checked hosts and ports; without them, the page still gets a policy.
    let csp = HeaderValue::try_from(page_csp(&services)).or_else(|_| HeaderValue::try_from(page_csp(&[])));
    if let Ok(csp) = csp {
        headers.insert(header::CONTENT_SECURITY_POLICY, csp);
    }
    response
}

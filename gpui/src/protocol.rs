//! WebView のカスタムプロトコル `planetext://` で `dist/` の静的ファイルを提供します。

use std::borrow::Cow;
use std::fs;
use std::path::PathBuf;
use wry::http::{header, Request, Response, StatusCode};

const ASSETS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../dist");

pub fn url() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "http://planetext.localhost/index.html"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "planetext://localhost/index.html"
    }
}

pub fn asset_response(
    _id: wry::WebViewId<'_>,
    request: Request<Vec<u8>>,
) -> Response<Cow<'static, [u8]>> {
    let path = request.uri().path();
    let trimmed = path.trim_start_matches('/');
    let relative = if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    };
    let safe = relative
        .split('/')
        .filter(|c| !c.is_empty() && *c != "..")
        .collect::<PathBuf>();

    let file = PathBuf::from(ASSETS_DIR).join(&safe);
    let real = match fs::canonicalize(&file) {
        Ok(p) => p,
        Err(_) => return not_found(),
    };
    let base = PathBuf::from(ASSETS_DIR);
    if !real.starts_with(base) {
        return not_found();
    }

    match fs::read(&real) {
        Ok(data) => {
            let mime = mime_type(&real);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                .body(Cow::Owned(data))
                .unwrap_or_else(|_| fallback_response())
        }
        Err(_) => not_found(),
    }
}

fn not_found() -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Cow::Borrowed(&b"not found"[..]))
        .unwrap_or_else(|_| fallback_response())
}

fn fallback_response() -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(Cow::Borrowed(&[] as &[u8]))
        .unwrap()
}

fn mime_type(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html",
        Some("js") | Some("mjs") => "application/javascript",
        Some("wasm") => "application/wasm",
        Some("css") => "text/css",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

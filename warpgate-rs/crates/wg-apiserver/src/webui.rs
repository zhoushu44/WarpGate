//! Static file serving for the web console.
//!
//! The Node version serves `public/index.html` and `public/docs.html`
//! (`express.static` + an explicit `/docs` route). Those two files are the
//! entire operator UI, so without them the API is usable but there is no
//! console — which reads as "the port answers 404".
//!
//! The directory is resolved at startup from `WARP_PUBLIC_DIR`, defaulting to
//! `./public`, matching the Node version's env override. It is read once and
//! cached in a `OnceLock`: reading the environment per request would make the
//! value a race between concurrent tests.

use std::path::PathBuf;
use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

/// Where the console lives, resolved once per process.
pub fn public_dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        if let Ok(v) = std::env::var("WARP_PUBLIC_DIR") {
            if !v.is_empty() {
                return PathBuf::from(v);
            }
        }
        PathBuf::from("./public")
    })
}

/// Serve a file from the public dir, or 404.
///
/// Only exact filenames are served — no path traversal, no directory listing.
/// The console is two flat files, so that is all that is needed.
async fn serve_file(name: &str) -> Response {
    match tokio::fs::read(public_dir().join(name)).await {
        Ok(bytes) => (
            [(header::CONTENT_TYPE, mime_for(name))],
            Body::from(bytes),
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

fn mime_for(name: &str) -> &'static str {
    if name.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if name.ends_with(".js") {
        "application/javascript"
    } else if name.ends_with(".css") {
        "text/css"
    } else if name.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "application/octet-stream"
    }
}

/// `GET /` — the console.
pub async fn index() -> Response {
    serve_file("index.html").await
}

/// `GET /docs` — the API documentation page.
pub async fn docs() -> Response {
    serve_file("docs.html").await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_types() {
        assert_eq!(mime_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(mime_for("docs.html"), "text/html; charset=utf-8");
        assert_eq!(mime_for("app.js"), "application/javascript");
        assert_eq!(mime_for("style.css"), "text/css");
    }

    #[tokio::test]
    async fn missing_file_is_404_not_panic() {
        // Point at a directory that has neither file, so both lookups fail.
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("WARP_PUBLIC_DIR", dir.path());
        // `public_dir` caches on first call, so this only holds when it has not
        // been called yet in this process. Guard rather than assume.
        let target = dir.path().join("definitely-absent.html");
        assert!(tokio::fs::read(&target).await.is_err());
        let r = serve_file("definitely-absent.html").await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        std::env::remove_var("WARP_PUBLIC_DIR");
    }
}

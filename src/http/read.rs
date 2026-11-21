//! The mobile reader's static files under `/read/` (reader design,
//! "Serving"), embedded at build time by `build.rs`, served with the
//! console's security headers. They hold no catalog data, so they need no
//! credential; the page reads `/api/v1` with the session cookie.

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;

use sha2::{Digest, Sha256};

use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::http::ui::{asset, console_file, security_headers};

// Pastes build.rs's generated `READ_ASSETS` here, as if written in this
// file (like a C `#include`).
include!(concat!(env!("OUT_DIR"), "/read_assets.rs"));

/// The console files the reader's page loads, precached with its own.
pub(crate) const SHARED_CONSOLE_FILES: &[&str] = &[
    "/ui/vendor/pico.min.css",
    "/ui/js/core/dom.js",
    "/ui/js/core/format.js",
];

/// A hash of `files`' names and bytes, as 16 hex digits.
///
/// # Arguments
/// * `files` - `(name, bytes)` pairs, in a fixed order.
///
/// # Returns
/// The same digits for the same files on every build with this toolchain
/// (`DefaultHasher::new` is SipHash with fixed keys).
#[tracing::instrument(level = "trace", skip_all)]
fn shell_version<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> String {
    let mut hasher = DefaultHasher::new();
    for (name, bytes) in files {
        name.hash(&mut hasher);
        bytes.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// A shared console file's bytes, by its URL (`/ui/...`); empty when the
/// console does not embed it (a test catches that).
#[tracing::instrument(level = "trace", skip_all)]
fn shared_bytes(url: &str) -> &'static [u8] {
    url.strip_prefix("/ui/")
        .and_then(console_file)
        .unwrap_or_default()
}

/// The shell's version: a hash of every file the worker precaches, the
/// reader's own and the shared console files, so a change to any of them
/// changes the worker's bytes and installed apps update. Computed once on
/// first use (`LazyLock` is like a module-level value computed on first
/// access).
static SHELL_VERSION: LazyLock<String> = LazyLock::new(|| {
    shell_version(
        READ_ASSETS.iter().copied().chain(
            SHARED_CONSOLE_FILES
                .iter()
                .map(|url| (*url, shared_bytes(url))),
        ),
    )
});

/// The worker's source with its version filled in, built once on first use.
static WORKER: LazyLock<String> = LazyLock::new(|| {
    let source = READ_ASSETS
        .iter()
        .find(|(name, _)| *name == "sw.js")
        .map(|(_, body)| String::from_utf8_lossy(body).into_owned())
        .unwrap_or_default();
    source.replace("__SHELL_VERSION__", &SHELL_VERSION)
});

/// The reader's routes.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "router builder: runs once at startup, outside any request, so a span would be an orphan root"
    )
)]
pub(crate) fn routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/read", get(|| async { Redirect::permanent("/read/") }))
        .route("/read/", get(|| async { asset(READ_ASSETS, "index.html") }))
        .route("/read/shell.json", get(shell))
        .route("/read/sw.js", get(worker))
        .route(
            "/read/{*path}",
            get(|Path(path): Path<String>| async move { asset(READ_ASSETS, &path) }),
        )
        .layer(middleware::map_response(security_headers))
}

/// `bytes`' SHA-256 as 64 lowercase hex digits, as the worker computes it
/// with `crypto.subtle.digest`.
#[tracing::instrument(level = "trace", skip_all)]
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            // `write!` into a String cannot fail; `let _` drops the Ok.
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// The worker's precache listing, built once: the shell version and each
/// file's URL with the SHA-256 of the bytes this build serves for it. The
/// worker installs only when the listing's version is its own and every file
/// it fetched hashes as listed, so a deploy between its requests (or a mix
/// of replicas on two builds) fails the install instead of caching a mix.
/// `index.html` is listed as `/read/`, the URL the page is opened at.
static SHELL_LISTING: LazyLock<serde_json::Value> = LazyLock::new(|| {
    let reader = READ_ASSETS.iter().map(|(name, body)| {
        let url = match *name {
            "index.html" => "/read/".to_string(),
            "sw.js" => "/read/sw.js".to_string(),
            other => format!("/read/{other}"),
        };
        // The worker's own bytes as served (version filled in).
        let bytes: &[u8] = if *name == "sw.js" {
            WORKER.as_bytes()
        } else {
            body
        };
        serde_json::json!({ "url": url, "sha256": sha256_hex(bytes) })
    });
    let shared = SHARED_CONSOLE_FILES
        .iter()
        .map(|url| serde_json::json!({ "url": url, "sha256": sha256_hex(shared_bytes(url)) }));
    serde_json::json!({ "version": SHELL_VERSION.as_str(), "files": reader.chain(shared).collect::<Vec<_>>() })
});

/// `GET /read/shell.json`: `{"version": ..., "files": [{"url", "sha256"}]}`,
/// what the worker precaches and how to check it.
#[tracing::instrument(level = "debug", skip_all)]
async fn shell() -> Json<serde_json::Value> {
    Json(SHELL_LISTING.clone())
}

/// `GET /read/sw.js`: the worker, with its version, revalidated on every
/// load so a deploy reaches installed apps on their next launch.
#[tracing::instrument(level = "debug", skip_all)]
async fn worker() -> Response {
    if WORKER.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        WORKER.as_str(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::testing::app;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// One GET through the whole router, as a browser would send it.
    async fn get(path: &str) -> Response {
        let test = app().await;
        crate::http::router(
            std::sync::Arc::clone(&test.app),
            &tokio_util::sync::CancellationToken::new(),
        )
        .oneshot(
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
    }

    /// The body as text.
    async fn text(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8(bytes.to_vec()).expect("utf-8")
    }

    #[tokio::test]
    async fn the_reader_page_is_served_with_the_console_security_headers() {
        let response = get("/read/").await;
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(headers["content-type"], "text/html; charset=utf-8");
        assert_eq!(
            headers["content-security-policy"],
            crate::http::ui::CONTENT_SECURITY_POLICY
        );
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers["referrer-policy"], "no-referrer");
        assert!(text(response).await.contains("/read/manifest.webmanifest"));
    }

    #[tokio::test]
    async fn bare_read_redirects_to_the_reader() {
        let response = get("/read").await;
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(response.headers()["location"], "/read/");
    }

    #[tokio::test]
    async fn the_manifest_and_icons_have_their_content_types() {
        assert_eq!(
            get("/read/manifest.webmanifest").await.headers()["content-type"],
            "application/manifest+json"
        );
        assert_eq!(
            get("/read/icons/icon-192.png").await.headers()["content-type"],
            "image/png"
        );
    }

    #[tokio::test]
    async fn the_reader_and_the_console_never_serve_each_others_files() {
        assert_eq!(
            get("/read/styles.css").await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get("/ui/manifest.webmanifest").await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(get("/read/nope.js").await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_shell_listing_names_every_reader_file_and_the_shared_ones() {
        let response = get("/read/shell.json").await;
        assert_eq!(response.status(), StatusCode::OK);
        let listing: serde_json::Value = serde_json::from_str(&text(response).await).expect("json");
        assert_eq!(listing["version"], SHELL_VERSION.as_str());
        let entries = listing["files"].as_array().expect("files");
        let files: Vec<&str> = entries
            .iter()
            .filter_map(|entry| entry["url"].as_str())
            .collect();
        assert_eq!(files.len(), entries.len(), "every entry has a url");
        assert!(files.contains(&"/read/"));
        assert!(!files.contains(&"/read/index.html"));
        assert!(files.contains(&"/read/manifest.webmanifest"));
        for shared in SHARED_CONSOLE_FILES {
            assert!(files.contains(shared), "{shared}");
        }
        // Each listed file is served, with exactly the bytes its hash
        // names, so a worker that verifies them installs.
        for entry in entries {
            let url = entry["url"].as_str().expect("url");
            let response = get(url).await;
            assert_eq!(response.status(), StatusCode::OK, "{url}");
            let bytes = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body");
            assert_eq!(entry["sha256"], sha256_hex(&bytes), "{url}");
        }
    }

    #[tokio::test]
    async fn the_worker_is_revalidated_and_carries_the_shell_version() {
        let response = get("/read/sw.js").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-cache");
        assert_eq!(
            response.headers()["content-type"],
            "text/javascript; charset=utf-8"
        );
        let source = text(response).await;
        assert!(source.contains(SHELL_VERSION.as_str()));
        assert!(!source.contains("__SHELL_VERSION__"));
    }

    #[tokio::test]
    async fn the_console_does_not_serve_the_reader_worker() {
        assert_eq!(get("/ui/sw.js").await.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn the_shell_version_changes_when_only_a_shared_file_does() {
        let old: [(&str, &[u8]); 2] = [("index.html", b"page"), ("/ui/js/core/dom.js", b"old")];
        let new: [(&str, &[u8]); 2] = [("index.html", b"page"), ("/ui/js/core/dom.js", b"new")];
        assert_ne!(shell_version(old), shell_version(new));
        assert_eq!(shell_version(old), shell_version(old));
    }

    #[test]
    fn the_served_version_covers_the_shared_console_files() {
        let reader_only = shell_version(READ_ASSETS.iter().copied());
        assert_ne!(*SHELL_VERSION, reader_only);
        for url in SHARED_CONSOLE_FILES {
            assert!(!shared_bytes(url).is_empty(), "{url} is embedded");
        }
    }
}

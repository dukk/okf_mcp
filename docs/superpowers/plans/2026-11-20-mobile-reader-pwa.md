# Mobile Reader PWA Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Read `AGENTS.md` and `tools/agentic/README.md` first: every gate runs through `tools/agentic/gate.sh`.

**Goal:** An installable, read-only web app at `/read/` for reading a catalog on a phone, with recently viewed concepts readable offline.

**Architecture:** The reader's static files live in `src/http/read/`, embedded by `build.rs` into their own table and served by a new `src/http/read.rs`, with the console's security headers. A service worker scoped to `/read/` precaches the shell and keeps network-first caches of concept and metadata reads (concepts capped at 200, least recently viewed out first). The page signs in with the console's session cookie, classifies every API answer, and does all cache upkeep (wipe on `401`, evict a catalog on lost access, evict a concept on `404`).

**Tech Stack:** Rust (axum, `build.rs` embedding), vanilla ES modules, Pico CSS, Service Worker and Cache Storage APIs, rustwright headless Chromium for browser journeys.

**Spec:** `docs/superpowers/specs/2026-11-20-mobile-reader-pwa-design.md`.

**Rules for every task (from `.claude/CLAUDE.md` and the frontend rules):**
- Pico CSS and vanilla JS only. No inline `style` attributes or inline scripts (the CSP forbids them). Data reaches the DOM through `h()` and `textContent`; the only `innerHTML` is `body_html`, as in the console.
- Rust: doc comments on every item, no `.unwrap()`, comments for Rust idioms a Python developer may not know, `#[tracing::instrument]` on functions (or the `hyp_tracing` allow attribute with a reason, as the surrounding code does).
- No real hostnames, domains, people or registries in any file; use `example.com`.
- Gates: `tools/agentic/gate.sh fmt`, `tools/agentic/gate.sh lint`, `tools/agentic/gate.sh test`, `tools/agentic/gate.sh test-ui`, `tools/agentic/gate.sh dylint`. Narrow runs that need the database: `scripts/test-pg.sh run cargo test --lib <filter>`.
- End every commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

---

## File structure

| File | Responsibility |
|---|---|
| `build.rs` (modify) | Embed `src/http/ui` as `ASSETS` and `src/http/read` as `READ_ASSETS`, through one shared function. |
| `src/http/ui.rs` (modify) | `asset()` takes the table it serves from; `console_file()` exposes one console file's bytes; `content_type()` knows `png` and `webmanifest`. |
| `src/http/read.rs` (create) | The reader's routes: `/read`, `/read/`, `/read/shell.json`, `/read/sw.js` (version substituted), `/read/{*path}`; the shell version, a hash of every precached file (the reader's and the shared console files). |
| `src/http/mod.rs` (modify) | Declare `mod read;` and merge `read::routes()`. |
| `src/http/pages.rs` (modify) | `return_to` accepts `/read/` paths. |
| `src/http/console_session.rs` (modify) | Every `/api/v1` answer carries `X-OKF-Account`, an opaque tag of the account it was made for. |
| `src/http/read/index.html` | The reader's page: head metadata, bar, banner, main, bottom tabs. |
| `src/http/read/manifest.webmanifest` | Install metadata. |
| `src/http/read/icons/*.png` | App icons (192, 512, maskable 512, Apple 180). |
| `src/http/read/read.css` | The reader's own tokens, light and dark, phone layout, body markup. |
| `src/http/read/sw.js` | The service worker. |
| `src/http/read/js/names.js` | Cache names and the cap, shared by page and worker (classic script). |
| `src/http/read/js/api.js` | Read-only client: `read`, `readAll`, `signOut`, `classify`, `KIND`, `ReadError`, `enc`, `encPath`. |
| `src/http/read/js/cache.js` | Page-side cache upkeep: `wipe`, `evictCatalog`, `evictDocument`, `offlineDocuments`, and the invalidation marks it sends the worker. (The worker keeps the owner record.) |
| `src/http/read/js/route.js` | Hash routes: `parse`, `hrefs`, `hrefFor`, `fromBrowse`, `parentOf`. |
| `src/http/read/js/main.js` | Boot, session, worker registration, screen dispatch, freshness banner, failure handling, tabs. |
| `src/http/read/js/parts.js` | Shared view parts: `searchForm`, `crumbs`, `renderBody`, `message`. |
| `src/http/read/js/listing.js` | `folderListing`: one folder's subfolders, index and concepts from the tree. |
| `src/http/read/js/screens/{home,catalogs,tree,search,concept,document,me}.js` | One screen each. |
| `tests/ui_e2e/harness.rs` (modify) | `open_reader`, `sign_in_reader`, `wait_worker`, `go_offline`. |
| `tests/ui_e2e/read.rs` (create), `tests/ui_e2e/main.rs` (modify) | The reader's browser journeys. |
| `docs/reference/reader.md` (create), `README.md` (modify) | How to install and what works offline. |
| `changelog/<timestamp>-mobile-reader.md` (create) | The change, per the project's changelog rule. |

---

### Task 1: Embed and serve the reader's files

**Files:**
- Modify: `build.rs`
- Modify: `src/http/ui.rs` (`asset`, `content_type`, tests)
- Create: `src/http/read.rs`
- Modify: `src/http/mod.rs` (the module list, and `open.merge(ui::routes())` near line 433)
- Create: `src/http/read/index.html`, `src/http/read/manifest.webmanifest`, `src/http/read/icons/` (4 PNGs)

- [ ] **Step 1: Generate the icons**

Write this script to the session scratchpad (not the repo) and run it with `python3 -I <script> src/http/read/icons`. It uses only the standard library: a rounded square in the reader's accent colour with three white "text lines".

```python
"""Writes the reader's PNG icons into the directory given as argv[1]."""
import os
import struct
import sys
import zlib

ACCENT = (47, 111, 94)
WHITE = (255, 255, 255)


def png(path, size, maskable):
    radius = 0 if maskable else size // 5
    pad = size // 4 if maskable else size // 5
    bar_h = max(2, size // 12)
    bars = [(pad + i * bar_h * 2, 1.0 if i < 2 else 0.6) for i in range(3)]
    top = (size - (bar_h * 5)) // 2
    rows = []
    for y in range(size):
        row = bytearray([0])
        for x in range(size):
            inside = True
            for cx, cy in ((radius, radius), (size - radius - 1, radius),
                           (radius, size - radius - 1), (size - radius - 1, size - radius - 1)):
                if (x < radius or x > size - radius - 1) and (y < radius or y > size - radius - 1):
                    if (x - cx) ** 2 + (y - cy) ** 2 > radius ** 2 and \
                            abs(x - cx) <= radius and abs(y - cy) <= radius:
                        inside = False
            colour, alpha = ACCENT, 255 if inside else 0
            for index, (_, share) in enumerate(bars):
                y0 = top + index * bar_h * 2
                if y0 <= y < y0 + bar_h and pad <= x < pad + int((size - 2 * pad) * share):
                    colour = WHITE
            row += bytes(colour) + bytes([alpha])
        rows.append(bytes(row))
    raw = zlib.compress(b"".join(rows), 9)

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + \
            struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    with open(path, "wb") as out:
        out.write(b"\x89PNG\r\n\x1a\n")
        out.write(chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)))
        out.write(chunk(b"IDAT", raw))
        out.write(chunk(b"IEND", b""))


target = sys.argv[1]
os.makedirs(target, exist_ok=True)
png(os.path.join(target, "icon-192.png"), 192, False)
png(os.path.join(target, "icon-512.png"), 512, False)
png(os.path.join(target, "icon-maskable-512.png"), 512, True)
png(os.path.join(target, "apple-touch-icon.png"), 180, True)
```

Run: `ls -l src/http/read/icons` and expect four PNG files. Open one with the Read tool to check it looks right.

- [ ] **Step 2: Write `src/http/read/manifest.webmanifest`**

```json
{
    "name": "Catalog Reader",
    "short_name": "Reader",
    "description": "Read your knowledge catalog on the go.",
    "start_url": "/read/",
    "scope": "/read/",
    "display": "standalone",
    "background_color": "#f7f7f4",
    "theme_color": "#2f6f5e",
    "icons": [
        { "src": "/read/icons/icon-192.png", "sizes": "192x192", "type": "image/png" },
        { "src": "/read/icons/icon-512.png", "sizes": "512x512", "type": "image/png" },
        { "src": "/read/icons/icon-maskable-512.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable" }
    ]
}
```

- [ ] **Step 3: Write `src/http/read/index.html`**

```html
<!doctype html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
    <title>Reader</title>
    <meta name="color-scheme" content="light dark">
    <meta name="theme-color" content="#f7f7f4" media="(prefers-color-scheme: light)">
    <meta name="theme-color" content="#11161a" media="(prefers-color-scheme: dark)">
    <meta name="apple-mobile-web-app-capable" content="yes">
    <meta name="apple-mobile-web-app-status-bar-style" content="default">
    <meta name="apple-mobile-web-app-title" content="Reader">
    <link rel="manifest" href="/read/manifest.webmanifest">
    <link rel="icon" href="/read/icons/icon-192.png" type="image/png">
    <link rel="apple-touch-icon" href="/read/icons/apple-touch-icon.png">
    <link rel="stylesheet" href="/ui/vendor/pico.min.css">
    <link rel="stylesheet" href="/read/read.css">
    <script src="/read/js/names.js"></script>
    <script type="module" src="/read/js/main.js"></script>
</head>
<body>
    <header class="bar">
        <a href="#/c" id="bar-catalog" class="bar-catalog">Reader</a>
    </header>
    <p id="offline-banner" class="banner" role="status" hidden></p>
    <main id="reader" class="reader" hidden></main>
    <nav class="tabs" aria-label="Reader">
        <a id="tab-home" href="#/">Home</a>
        <a id="tab-browse" href="#/c">Browse</a>
        <a id="tab-search" href="#/c">Search</a>
        <a id="tab-me" href="#/me">Account</a>
    </nav>
    <noscript>The reader needs JavaScript.</noscript>
</body>
</html>
```

- [ ] **Step 4: Write the failing Rust tests in `src/http/read.rs`**

Create `src/http/read.rs` with only its test module for now (the module is declared in Step 6):

```rust
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
        .oneshot(Request::builder().uri(path).body(Body::empty()).expect("request"))
        .await
        .expect("response")
    }

    /// The body as text.
    async fn text(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
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
        assert_eq!(get("/read/styles.css").await.status(), StatusCode::NOT_FOUND);
        assert_eq!(get("/ui/manifest.webmanifest").await.status(), StatusCode::NOT_FOUND);
        assert_eq!(get("/read/nope.js").await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_shell_listing_names_every_reader_file_and_the_shared_ones() {
        let response = get("/read/shell.json").await;
        assert_eq!(response.status(), StatusCode::OK);
        let listing: serde_json::Value =
            serde_json::from_str(&text(response).await).expect("json");
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
            let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
            assert_eq!(entry["sha256"], sha256_hex(&bytes), "{url}");
        }
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
```

- [ ] **Step 5: Run the tests to see them fail**

Run: `scripts/test-pg.sh run cargo test --lib http::read`
Expected: a compile error (`read` is not a module, `SHELL_VERSION` and `shell_version` not found).

- [ ] **Step 6: Embed the reader in `build.rs`**

Replace the body of `build.rs` from `fn main()` to the end of `main` with this, keeping `collect` and the file's top doc comment (update its second bullet to also mention `src/http/read/**` as `$OUT_DIR/read_assets.rs`):

```rust
fn main() {
    println!("cargo:rerun-if-changed=migrations");
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    embed(&manifest, "src/http/ui", "ASSETS", &out_dir.join("ui_assets.rs"));
    embed(&manifest, "src/http/read", "READ_ASSETS", &out_dir.join("read_assets.rs"));
}

/// Writes `dest`: a `const <table>: &[(&str, &[u8])]` of every file under
/// `tree`, by its path below it, sorted so the binary is reproducible.
fn embed(manifest: &str, tree: &str, table: &str, dest: &Path) {
    // A directory here makes cargo rescan it recursively for changes.
    println!("cargo:rerun-if-changed={tree}");
    let root = Path::new(manifest).join(tree);
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.sort();
    let mut out = format!(
        "/// Every file under `{tree}`, by its path below it (generated by build.rs).\n\
         const {table}: &[(&str, &[u8])] = &[\n"
    );
    for (relative, absolute) in &files {
        out.push_str(&format!(
            "    ({relative:?}, include_bytes!({absolute:?})),\n"
        ));
    }
    out.push_str("];\n");
    fs::write(dest, out).expect("write an embedded asset table");
}
```

Change the `std::path` import at the top of `build.rs` to `use std::path::{Path, PathBuf};`. In `collect`, change the two messages `"read src/http/ui"` and `"a file below src/http/ui"` to `"read an embedded tree"` and `"a file below its tree"`.

- [ ] **Step 7: Let `src/http/ui.rs` serve from any table**

Change `asset` to take the table, and add the two content types:

```rust
/// One embedded file from `table`, revalidated on every load (`no-cache`),
/// or 404.
///
/// # Arguments
/// * `table` - the embedded files to look in ([`ASSETS`] or the reader's).
///   The bytes are `'static` (they live in the binary), which axum needs to
///   send them without copying.
/// * `path` - the file's path below its tree.
#[tracing::instrument(level = "trace", skip_all)]
pub(crate) fn asset(table: &[(&str, &'static [u8])], path: &str) -> Response {
    match table.iter().find(|(name, _)| *name == path) {
        Some((name, body)) => (
            [
                (header::CONTENT_TYPE, content_type(name)),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            *body,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
```

In `routes()`, the two calls become `asset(ASSETS, "index.html")` and `asset(ASSETS, &path)`. Add, after `asset`, the accessor the reader uses to version the console files it shares:

```rust
/// One console file's bytes, by its path below `src/http/ui`.
///
/// # Returns
/// `None` when the console embeds no such file.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
    )
)]
pub(crate) fn console_file(path: &str) -> Option<&'static [u8]> {
    ASSETS
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, body)| *body)
}
``` In `content_type`, add before the `_` arm:

```rust
        Some("png") => "image/png",
        Some("webmanifest") => "application/manifest+json",
```

Add to the `content_types_follow_the_extension` test:

```rust
        assert_eq!(content_type("icons/a.png"), "image/png");
        assert_eq!(content_type("manifest.webmanifest"), "application/manifest+json");
```

- [ ] **Step 8: Write the routes in `src/http/read.rs`**

Put this above the test module:

```rust
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
        let bytes: &[u8] = if *name == "sw.js" { WORKER.as_bytes() } else { body };
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
```

(`sw.js` does not exist until Task 3, so `/read/sw.js` answers 404 until then; Task 3 tests it.)

- [ ] **Step 9: Register the module**

> **Corrected (issue #808, 2026-11-20).** The plan's line numbers (`:18`, `:413`) were stale: `ui::routes()` is near line 433 of `src/http/mod.rs`. Fixed inline; no other task refers to them.

In `src/http/mod.rs`, add `mod read;` beside `mod ui;` (rustfmt sorts it into the list), and change `open.merge(ui::routes())` (near line 433) to `open.merge(ui::routes()).merge(read::routes())`.

- [ ] **Step 10: Run the tests to see them pass**

Run: `scripts/test-pg.sh run cargo test --lib http::read` and then `scripts/test-pg.sh run cargo test --lib http::ui` (cargo takes one test filter per run).
Expected: PASS, every test in both modules.

- [ ] **Step 11: Gates and commit**

Run: `tools/agentic/gate.sh fmt` then `tools/agentic/gate.sh lint`. Expected: both exit 0.

```bash
git add build.rs src/http/ui.rs src/http/read.rs src/http/mod.rs src/http/read
git commit -m "feat(read): embed and serve the mobile reader's files under /read/

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Server support: sign-in returns to the reader, answers name their account

**Files:**
- Modify: `src/http/pages.rs:195-205` (`is_safe_return_to`) and its tests (`:317-339`)
- Modify: `src/http/console_session.rs` (`ACCOUNT_HEADER`, `account_tag`, `require_principal`, tests)

- [ ] **Step 1: Write the failing test**

In `return_to_keeps_same_origin_console_and_authorize_paths`, extend the list:

```rust
        for kept in [
            "/ui/",
            "/ui/#/keys",
            "/oauth/authorize?client_id=x&state=y",
            "/read/",
            "/read/#/v/main/systems/alpha.md",
        ] {
```

In `return_to_falls_back_to_the_console_for_anything_else`, add to the refused list:

```rust
            Some("/read"),
            Some("/readx/"),
            Some("/read/\\evil"),
            Some("/read/ x"),
```

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test --lib http::pages::tests::return_to`
Expected: FAIL in `return_to_keeps_same_origin_console_and_authorize_paths` for `"/read/"`.

- [ ] **Step 3: Accept the reader's prefix**

```rust
/// Whether `path` is a same-origin console, reader or authorize path.
#[tracing::instrument(level = "trace", skip_all)]
fn is_safe_return_to(path: &str) -> bool {
    // Visible ASCII only: no spaces, no control characters (a header
    // injection) and no `\`, which some browsers read as `/` (so `/\host`
    // would leave the origin). Every prefix starts with one `/` followed by
    // a path segment, so the result can never be `//host`.
    path.bytes()
        .all(|byte| byte.is_ascii_graphic() && byte != b'\\')
        && (path.starts_with("/ui/")
            || path.starts_with("/read/")
            || path.starts_with("/oauth/authorize?"))
}
```

Update the doc comment of `safe_return_to` ("a console path, a reader path or an authorize URL on this origin").

- [ ] **Step 4: Run it to see it pass**

Run: `cargo test --lib http::pages::tests::return_to`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/http/pages.rs
git commit -m "feat(read): sign-in may return to /read/

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 6: Tag every `/api/v1` answer with its account**

> **Corrected (issue #809, 2026-11-20).** `account_tag` takes an `Account`
> (`Account::User(&row.user_id)` for a session, `Account::Key(&principal.key_id)`
> for a bearer credential) and hashes `okf-account:user:<id>` or
> `okf-account:key:<id>`, so a key whose id happens to equal a user's id never
> shares that user's tag. The code below is the shipped form. The header on
> real answers, and its absence from the middleware's own 401 and 403, is
> checked by `console_session::http_tests::api_answers_name_their_account_and_refusals_do_not`.

The reader shares the console's cookie, so another tab can sign in as someone else while a reader tab stays open. Its service worker must know whose answer each response is, so it never files one account's answers under another's. In `src/http/console_session.rs`, add after the `CSRF_HEADER` constant:

```rust
/// The header that names, opaquely, the account an `/api/v1` answer was
/// made for (a user for a session, the key for a bearer credential), so the
/// mobile reader's worker never files one account's answers under another's.
pub const ACCOUNT_HEADER: &str = "x-okf-account";

/// Whose answer an `/api/v1` response is, for [`account_tag`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Account<'a> {
    /// A console session's user, by user id.
    User(&'a str),
    /// A bearer credential (API key or OAuth grant), by key id.
    Key(&'a str),
}

/// An opaque, stable tag for `account`: 16 hex digits of SHA-256 of a
/// prefixed id, so the header names no user and no key.
#[tracing::instrument(level = "trace", skip_all)]
pub(crate) fn account_tag(account: Account<'_>) -> HeaderValue {
    let prefixed = match account {
        Account::User(id) => format!("okf-account:user:{id}"),
        Account::Key(id) => format!("okf-account:key:{id}"),
    };
    let digest = Sha256::digest(prefixed.as_bytes());
    let hex = digest.iter().take(8).fold(String::with_capacity(16), |mut hex, byte| {
        // `write!` into a String cannot fail; `let _` drops the Ok.
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    HeaderValue::from_str(&hex).expect("hex digits are a valid header value")
}
```

with `use std::fmt::Write as _;` and `use sha2::{Digest, Sha256};` among the imports. In `require_principal`, the bearer branch's `return next.run(request).await;` becomes:

```rust
            let tag = account_tag(Account::Key(&principal.key_id));
            let mut response = next.run(request).await;
            response.headers_mut().insert(ACCOUNT_HEADER, tag);
            return response;
```

(take `tag` before `principal` is moved into the extensions), and the session branch's final `next.run(request).await` becomes:

```rust
    let mut response = next.run(request).await;
    response.headers_mut().insert(ACCOUNT_HEADER, account_tag(Account::User(&row.user_id)));
    response
```

(shipped: `let tag = account_tag(Account::User(&row.user_id));` is taken before the `ConsoleSession` insert, and the insert after `next.run` uses `tag`.)

Add to the module's tests:

```rust
    #[test]
    fn account_tags_are_stable_opaque_and_distinct() {
        let alice = account_tag(Account::User("user-alice"));
        assert_eq!(alice, account_tag(Account::User("user-alice")));
        assert_ne!(alice, account_tag(Account::User("user-bob")));
        assert_ne!(alice, account_tag(Account::Key("user-alice")));
        let text = alice.to_str().expect("ascii");
        assert_eq!(text.len(), 16);
        assert!(text.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(!text.contains("alice"));
    }
```

- [ ] **Step 7: Run it and commit**

Run: `cargo test --lib http::console_session::tests::account_tags`
Expected: PASS. (The header on real answers is checked by `http_tests::api_answers_name_their_account_and_refusals_do_not` here, and by the two-account journey in Task 11.)

```bash
git add src/http/console_session.rs
git commit -m "feat(read): /api/v1 answers carry an opaque account tag

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: The service worker

**Files:**
- Create: `src/http/read/js/names.js`, `src/http/read/sw.js`
- Modify: `src/http/read.rs` (tests)

- [ ] **Step 1: Write the failing test**

Add to `src/http/read.rs`'s tests:

```rust
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
```

- [ ] **Step 2: Run it to see it fail**

Run: `scripts/test-pg.sh run cargo test --lib http::read`
Expected: FAIL, `/read/sw.js` answers 404.

- [ ] **Step 3: Write `src/http/read/js/names.js`**

```js
// The reader's cache names and the concept cap, written once for both the
// page (index.html loads this as a classic script) and the worker (sw.js
// loads it with importScripts). `self` is the window in one and the worker
// in the other.
self.READ_CACHES = Object.freeze({
    shellPrefix: 'read-shell-',
    content: 'read-content',
    meta: 'read-meta',
    owner: 'read-owner',
    contentCap: 200,
});
```

- [ ] **Step 4: Write `src/http/read/sw.js`**

> **Corrected (issue #810, 2026-11-20).** The listing below is the worker as shipped; it replaces
> the earlier text, which never kept an answer that made an account the owner, nor any answer
> on its way with it: the takeover set a `'*'` mark at its own time, which every request that
> had already started (the first screen of a first visit, journeys 8m and 8q) then matched, and
> the request-time `caches.open` handle wrote into a cache the takeover had just deleted. Now an
> owner change bumps an `epoch` instead (a copy is kept only while the epoch it was admitted
> under is current, checked before and after the write), `store` opens the cache at write time,
> marks and `since` use the monotonic `performance.now()` (so `since` is not stored: a record
> read from `read-owner` is `{ tag }`, and no request of the reading worker predates it), a
> failed takeover disowns the copies, and a half-written shell cache is deleted. `remember` is
> now `store(cacheName, key, response, { startedAt, held, capped })`; `admit` returns the epoch
> or null. Nothing in a later task calls these: the page only deletes `read-owner`, and journey
> 8q reads its `.tag`, which is unchanged.
>
> **Corrected (issue #810, 2026-11-20, review).** `takeOver` now deletes the old owner record with the old
> copies and keeps the new account's answers only once its own record is written; if any of
> that fails, it disowns and keeps nothing. Before, a failed record write left the old account's
> record naming the new account's copies, which a restarted worker then served to the old account.
> The worker is covered by `tests/read_worker/sw.test.mjs` (`just test-worker`, run by `just test`).

```js
// The reader's service worker (reader design, "Caching"). Its scope is
// /read/, so it never controls a console page. The shell is precached and
// served cache-first; concept and document reads (read-content, capped) and
// listings (read-meta) are network-first with the cache as the offline
// fallback. Only 2xx answers are cached. The worker never makes an error
// answer of its own; the page decides what every refusal means.
importScripts('/read/js/names.js');

const VERSION = '__SHELL_VERSION__';
const SHELL = `${READ_CACHES.shellPrefix}${VERSION}`;
const CONTENT = /^\/api\/v1\/catalogs\/[^/]+\/(concepts|documents)\//;
const META = [
    /^\/api\/v1\/catalogs$/,
    /^\/api\/v1\/catalogs\/[^/]+\/(tree|recent-changes)$/,
    /^\/api\/v1\/catalogs\/[^/]+\/links\//,
];
const CATALOG = /^\/api\/v1\/catalogs\/([^/]+)\//;
const OWNER_KEY = '/read/__owner';

// The worker's own clock for ordering requests against marks and owner
// changes. It is monotonic (a wall-clock change cannot reorder them) and
// lives as long as this worker, as the marks and the in-memory owner do.
const now = () => performance.now();

// Installs only a shell that is entirely this build's: the listing must
// carry this worker's version, and every file must hash as listed. A deploy
// between these requests, or replicas on two builds, fails the install; the
// browser keeps the worker it had and tries again on a later visit.
self.addEventListener('install', event => {
    event.waitUntil((async () => {
        const answer = await fetch('/read/shell.json', { cache: 'no-store' });
        if (!answer.ok) throw new Error(`shell listing: HTTP ${answer.status}`);
        const listing = await answer.json();
        if (listing.version !== VERSION) throw new Error('the shell listing is from another build');
        const responses = await Promise.all(listing.files.map(verified));
        const cache = await caches.open(SHELL);
        try {
            await Promise.all(listing.files.map((file, at) => cache.put(file.url, responses[at])));
        } catch (failure) {
            // A half-written shell is never used; leave no copy of it.
            await caches.delete(SHELL).catch(() => {});
            throw failure;
        }
        await self.skipWaiting();
    })());
});

// `file.url`'s answer, refused unless its bytes hash as `file.sha256`.
async function verified(file) {
    const response = await fetch(file.url, { cache: 'no-store' });
    if (!response.ok) throw new Error(`shell file ${file.url}: HTTP ${response.status}`);
    const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', await response.clone().arrayBuffer()));
    const hex = [...digest].map(byte => byte.toString(16).padStart(2, '0')).join('');
    if (hex !== file.sha256) throw new Error(`shell file ${file.url} is from another build`);
    return response;
}

self.addEventListener('activate', event => {
    event.waitUntil((async () => {
        const names = await caches.keys().catch(() => []);
        await Promise.all(names
            .filter(name => name.startsWith(READ_CACHES.shellPrefix) && name !== SHELL)
            .map(name => caches.delete(name).catch(() => false)));
        await self.clients.claim();
    })());
});

// Invalidation marks, in memory: the page sends one ('*' for a wipe, an
// encoded catalog for a lost grant, or one document's API path for a
// deleted document) and waits for the reply before it deletes anything.
// The worker then refuses to store an answer to a request that started at
// or before a matching mark, so a slow 2xx cannot put back what a 401 or a
// lost grant removed. Memory needs no storage quota, and a request still on
// its way keeps the worker alive, so the marks last as long as they matter.
// (The Service Worker spec's Try Activate starts a new worker only once this
// one has no events pending, skipWaiting or not, so one worker at a time
// holds the marks that matter; a request the browser times out is the
// exception, and its answer is then no longer this worker's to keep.)
const marks = new Map();

// The account the cached copies belong to, { tag, since }, read from
// read-owner on first use (`undefined` until then). `tag` is null when no
// account owns them; `since` is when the owner took over, on `now()`'s
// clock (-Infinity for a record this worker read: no request of its own
// predates it). The worker, which every reader tab shares, decides
// ownership, so a sign-in as someone else in another tab cannot slip one
// account's answers in among another's.
let owner;
// Bumped whenever the owner changes. A copy is kept only while the epoch it
// was admitted under is current, so an answer admitted for the old owner
// drops its copy when a new one takes over, without touching the new
// owner's answers already on their way.
let epoch = 0;
// Ownership decisions run one at a time, in arrival order.
let ownerQueue = Promise.resolve(null);

// No account owns the copies from now on; answers to requests that started
// before this are never kept.
function disown() {
    owner = { tag: null, since: now() };
    epoch += 1;
}

self.addEventListener('message', event => {
    const scope = event.data?.type === 'invalidate' ? event.data.scope : undefined;
    if (typeof scope !== 'string') return;
    marks.set(scope, now());
    // A wipe also deletes read-owner: the next account to answer owns the
    // copies afresh.
    if (scope === '*') disown();
    event.ports[0]?.postMessage('ok');
});

self.addEventListener('fetch', event => {
    const request = event.request;
    if (request.method !== 'GET') return;
    const url = new URL(request.url);
    if (url.origin !== self.location.origin) return;
    if (request.mode === 'navigate' && url.pathname.startsWith('/read/')) {
        event.respondWith(fromShell(new Request('/read/'), request));
    } else if (CONTENT.test(url.pathname)) {
        event.respondWith(networkFirst(request, READ_CACHES.content, true));
    } else if (META.some(pattern => pattern.test(url.pathname))) {
        event.respondWith(networkFirst(request, READ_CACHES.meta, false));
    } else if (!url.pathname.startsWith('/api/')) {
        event.respondWith(fromShell(request, request));
    }
    // Any other API read (session, search, me) goes to the network as is.
});

// The shell's copy of `key`, else the network's answer to `request`.
// Storage that refuses to read means the network's answer.
async function fromShell(key, request) {
    const cached = await caches.match(key, { cacheName: SHELL }).catch(() => undefined);
    return cached ?? fetch(request);
}

// The network's answer, remembered when it is a 2xx; without a network,
// the remembered copy marked X-Read-Offline. `capped` caches keep only the
// newest READ_CACHES.contentCap entries. Remembering is best effort: a
// failed write (a full disk, QuotaExceededError, storage that refuses to
// open) never costs the page the answer it already has.
async function networkFirst(request, cacheName, capped) {
    const startedAt = now();
    let response;
    try {
        response = await fetch(request);
    } catch (failure) {
        const held = epoch;
        const cached = await caches.open(cacheName)
            .then(cache => cachedCopy(cache, request))
            .catch(() => null);
        if (!cached) throw failure;
        // Re-put so the copy counts as recently viewed; a renewal, never trimmed.
        if (capped) {
            await upkeep(() => store(cacheName, cached.key, cached.response.clone(), { startedAt, held, capped: false }));
        }
        return stamped(cached.response, 'X-Read-Offline', '1');
    }
    if (response.ok) {
        const copy = stamped(response.clone(), 'X-Read-Cached-At', new Date().toISOString());
        await upkeep(async () => {
            if (invalidatedSince(request.url, startedAt)) return;
            const held = await admit(response.headers.get('X-OKF-Account'), startedAt);
            if (held !== null) await store(cacheName, request, copy, { startedAt, held, capped });
        });
    }
    return response;
}

async function loadOwner() {
    try {
        const stored = await (await caches.open(READ_CACHES.owner)).match(OWNER_KEY);
        const record = stored ? await stored.json() : null;
        return { tag: typeof record?.tag === 'string' ? record.tag : null, since: -Infinity };
    } catch {
        return { tag: null, since: -Infinity };
    }
}

// The epoch under which an answer for account `tag`, to a request that
// started at `startedAt`, may be cached, or null when it may not. The first
// answer for a new account makes it the owner (`takeOver`). An answer for
// another account to a request from before the current owner took over is
// never kept, so a late answer for the old account cannot take the copies
// back.
function admit(tag, startedAt) {
    const decision = ownerQueue.then(async () => {
        if (!tag) return null;
        if (owner === undefined) {
            const stored = await loadOwner();
            // A wipe may have disowned the copies while the record was read.
            if (owner === undefined) owner = stored;
        }
        if (owner.tag === tag) return epoch;
        if (startedAt <= owner.since) return null;
        return takeOver(tag);
    });
    ownerQueue = decision.catch(() => null);
    return ownerQueue;
}

// Makes `tag` the owner. The epoch is bumped first, so the old owner's
// copies still on their way drop themselves. Then the old owner's copies and
// its record go, and only then is the new owner recorded; `tag`'s answers
// are kept only once that record is written. So the durable record never
// names an account other than the one whose copies are cached: at worst
// there is no record, and a restarted worker then treats the next answer as
// a new owner and wipes. Returns the new epoch, or null (keep nothing) when
// the old state could not be deleted, the record could not be written, or a
// wipe came meanwhile.
async function takeOver(tag) {
    owner = { tag, since: now() };
    epoch += 1;
    const mine = epoch;
    try {
        await Promise.all([READ_CACHES.content, READ_CACHES.meta, READ_CACHES.owner]
            .map(name => caches.delete(name)));
        if (epoch !== mine) return null;
        const record = await caches.open(READ_CACHES.owner);
        await record.put(OWNER_KEY, new Response(JSON.stringify({ tag })));
    } catch {
        // Whatever is left belongs to nobody, and this answer is not kept;
        // the next request tries again.
        if (epoch === mine) disown();
        return null;
    }
    return epoch === mine ? mine : null;
}

// Whether the page wiped everything, or evicted `url`'s catalog or the
// document itself (any form of it: the mark is its path, without query),
// at or after `startedAt`.
function invalidatedSince(url, startedAt) {
    const path = new URL(url, self.location.origin).pathname;
    const catalog = CATALOG.exec(path)?.[1];
    return [marks.get('*'), catalog ? marks.get(catalog) : undefined, marks.get(path)]
        .some(at => at !== undefined && at >= startedAt);
}

// Stores `response` under `key` in `cacheName`, unless the page invalidated
// it since `startedAt` or the owner changed since epoch `held`. The cache is
// opened here, not when the request started: a takeover or a wipe deletes
// the cache, and a handle from before would write where nobody reads. The
// checks run again after the write: the page marks before it deletes, and a
// takeover bumps the epoch before it deletes, so an eviction that began
// meanwhile either finds this entry and deletes it, or changed the state
// before this second check, which deletes it.
async function store(cacheName, key, response, { startedAt, held, capped }) {
    const url = key.url ?? key;
    const current = () => epoch === held && !invalidatedSince(url, startedAt);
    if (!current()) return;
    const cache = await caches.open(cacheName);
    await renew(cache, key, response);
    if (!current()) {
        await cache.delete(key);
        return;
    }
    if (capped) await trim(cache);
}

// The cached answer to `request`, or, for a concept, the other form of it
// that holds the same body: a `?full=true` copy answers a plain request, and
// a plain copy answers `?full=true` when it carries the whole body (it was
// not shortened). Returns { key, response } or null.
async function cachedCopy(cache, request) {
    const exact = await cache.match(request);
    if (exact) return { key: request, response: exact };
    const url = new URL(request.url);
    if (!/\/concepts\//.test(url.pathname)) return null;
    if (url.searchParams.get('full') === 'true') {
        url.searchParams.delete('full');
        const plain = await cache.match(url.href);
        const body = plain ? await plain.clone().json().catch(() => null) : null;
        return body?.body_html ? { key: url.href, response: plain } : null;
    }
    url.searchParams.set('full', 'true');
    const full = await cache.match(url.href);
    return full ? { key: url.href, response: full } : null;
}

// Runs one cache write, logging rather than throwing when it fails.
async function upkeep(write) {
    try {
        await write();
    } catch (error) {
        console.warn('reader cache write failed:', error?.name);
    }
}

// Stores `response` for `request` as the cache's newest entry. A cache keeps
// its entries in insertion order, so deleting before putting moves a
// re-read entry to the end, and the front holds the least recently viewed.
async function renew(cache, request, response) {
    await cache.delete(request);
    await cache.put(request, response);
}

// Deletes the oldest entries beyond the cap.
async function trim(cache) {
    const keys = await cache.keys();
    const excess = keys.length - READ_CACHES.contentCap;
    if (excess > 0) await Promise.all(keys.slice(0, excess).map(key => cache.delete(key)));
}

// `response` with one more header (a cached Response's headers are frozen).
function stamped(response, name, value) {
    const headers = new Headers(response.headers);
    headers.set(name, value);
    return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
}
```

- [ ] **Step 5: Run the tests to see them pass**

Run: `scripts/test-pg.sh run cargo test --lib http::read`
Expected: PASS. The `shell.json` test now also fetches `/read/sw.js` and `/read/js/names.js`.

- [ ] **Step 6: Commit**

```bash
git add src/http/read.rs src/http/read/sw.js src/http/read/js/names.js
git commit -m "feat(read): service worker with shell, content and meta caches

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: The reader's stylesheet

**Files:**
- Create: `src/http/read/read.css`

- [ ] **Step 1: Write `src/http/read/read.css`**

```css
/* The mobile reader's own look, on Pico CSS 2 (/ui/vendor/pico.min.css).
 * Independent of the console's styles.css: only Pico is shared. Tokens on
 * :root, light by default, dark from the system setting. Body markup is
 * what src/render.rs writes into body_html. */
:root {
    --read-bg: #f7f7f4;
    --read-surface: #ffffff;
    --read-text: #1d2327;
    --read-muted: #5b6770;
    --read-border: #dfe3e0;
    --read-accent: #2f6f5e;
    --read-accent-text: #ffffff;
    --read-error: #b3261e;
    --read-measure: 40rem;
    --read-touch: 2.75rem;
    --read-gap: 1rem;
    --read-radius: .6rem;
    --read-tab-height: 3.5rem;

    --pico-font-family: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
    --pico-font-size: 106.25%;
    --pico-line-height: 1.55;
    --pico-background-color: var(--read-bg);
    --pico-color: var(--read-text);
    --pico-muted-color: var(--read-muted);
    --pico-muted-border-color: var(--read-border);
    --pico-primary: var(--read-accent);
    --pico-primary-background: var(--read-accent);
    --pico-primary-inverse: var(--read-accent-text);
    --pico-border-radius: var(--read-radius);
    --pico-spacing: var(--read-gap);
}

@media (prefers-color-scheme: dark) {
    :root {
        --read-bg: #11161a;
        --read-surface: #1a2126;
        --read-text: #e6e9e7;
        --read-muted: #9aa6ad;
        --read-border: #2c363c;
        --read-accent: #6cc2a8;
        --read-accent-text: #0d1a16;
        --read-error: #f2b8b5;
    }
}

body {
    background: var(--read-bg);
    color: var(--read-text);
    min-height: 100vh;
    padding: env(safe-area-inset-top) env(safe-area-inset-right)
        calc(var(--read-tab-height) + env(safe-area-inset-bottom)) env(safe-area-inset-left);
}

.bar {
    position: sticky; top: 0; z-index: 2;
    display: flex; align-items: center;
    min-height: var(--read-touch);
    padding: .25rem var(--read-gap);
    background: var(--read-bg);
    border-bottom: 1px solid var(--read-border);
}
.bar-catalog { font-weight: 600; text-decoration: none; color: var(--read-text); }

.banner {
    margin: 0; padding: .5rem var(--read-gap);
    background: var(--read-surface); color: var(--read-muted);
    border-bottom: 1px solid var(--read-border); font-size: .9rem;
}

.reader { max-width: var(--read-measure); margin: 0 auto; padding: var(--read-gap); }
.reader h1 { font-size: 1.6rem; margin-bottom: .5rem; }
.reader h2 { font-size: 1.15rem; margin-top: 1.5rem; }

.tabs {
    position: fixed; bottom: 0; left: 0; right: 0; z-index: 2;
    display: grid; grid-template-columns: repeat(4, 1fr);
    height: calc(var(--read-tab-height) + env(safe-area-inset-bottom));
    padding-bottom: env(safe-area-inset-bottom);
    background: var(--read-surface); border-top: 1px solid var(--read-border);
}
.tabs a {
    display: flex; align-items: center; justify-content: center;
    min-height: var(--read-touch); text-decoration: none;
    color: var(--read-muted); font-size: .9rem;
}
.tabs a[aria-current="page"] { color: var(--read-accent); font-weight: 600; }

.list { list-style: none; padding: 0; margin: 0; }
.list li { list-style: none; border-bottom: 1px solid var(--read-border); padding: 0; margin: 0; }
.list a, .list button.row {
    display: block; width: 100%; min-height: var(--read-touch);
    padding: .65rem 0; text-align: left; text-decoration: none;
    color: var(--read-text); background: none; border: 0; border-radius: 0;
}
.list .meta { display: block; color: var(--read-muted); font-size: .85rem; }
.folder::before { content: "/ "; color: var(--read-muted); }

.crumbs ol { display: flex; flex-wrap: wrap; gap: .25rem; padding: 0; margin: 0 0 .5rem; font-size: .85rem; }
.crumbs li { list-style: none; color: var(--read-muted); }
.crumbs li + li::before { content: "/ "; }

.notice { padding: var(--read-gap); background: var(--read-surface); border: 1px solid var(--read-border); border-radius: var(--read-radius); }
.hint { font-size: .9rem; color: var(--read-muted); }

details.outline { margin: .5rem 0 1rem; }
details.outline li { list-style: none; }
details.outline .level-3 { padding-left: 1rem; }
details.outline .level-4 { padding-left: 2rem; }

.markdown { overflow-wrap: anywhere; }
.markdown pre { background: var(--read-surface); padding: .75rem; border-radius: var(--read-radius); overflow-x: auto; font-size: .85rem; }
.markdown code { font-size: .9em; }
.markdown table { display: block; overflow-x: auto; width: auto; font-size: .9rem; }
.markdown img { max-width: 100%; height: auto; }
.markdown blockquote { border-left: 3px solid var(--read-border); padding-left: var(--read-gap); color: var(--read-muted); }
.markdown a[title="Not in this catalog"] { color: var(--read-error); text-decoration: line-through; }
.markdown [data-slug] { scroll-margin-top: calc(var(--read-touch) + .5rem); }
```

- [ ] **Step 2: Check it is served**

Run: `scripts/test-pg.sh run cargo test --lib http::read::tests::the_shell_listing`
Expected: PASS (the listing now includes `/read/read.css`, and it is served).

- [ ] **Step 3: Commit**

```bash
git add src/http/read/read.css
git commit -m "feat(read): the reader's own stylesheet, light and dark

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Client modules (api, cache, route, listing, parts)

These are plain modules with no screen yet. They are exercised by the browser journeys from Task 6 on.

**Files:**
- Create: `src/http/read/js/api.js`, `cache.js`, `route.js`, `listing.js`, `parts.js`

- [ ] **Step 1: Write `src/http/read/js/api.js`**

```js
// The reader's one way to the server: GET /api/v1 with the session cookie
// (the browser sends it, same origin). Every refusal becomes a ReadError
// whose `kind` says what it means for the reader (reader design, "Error
// handling"); main.js decides what to do with it.

export const KIND = Object.freeze({
    SIGNED_OUT: 'signed-out',
    CATALOG_GONE: 'catalog-gone',
    CONCEPT_GONE: 'concept-gone',
    LOCKED: 'locked',
    UNAVAILABLE: 'unavailable',
    FAILED: 'failed',
});

export class ReadError extends Error {
    constructor(kind, message, body = null) {
        super(message);
        this.kind = kind;
        this.body = body;
    }
}

export const enc = encodeURIComponent;

// A catalog path ('/a/b.md' or 'a/b.md') as encoded URL segments.
export function encPath(path) {
    return String(path).replace(/^\/+/, '').split('/').map(enc).join('/');
}

// What a refusal means, by status and the body's error kind (the server's
// status mapping is in src/http/mod.rs, AppError::status).
export function classify(status, body) {
    const kind = body?.error;
    if (status === 401) return KIND.SIGNED_OUT;
    if (status === 403 && (kind === 'UnlockRequired' || kind === 'Suspended')) return KIND.LOCKED;
    if (status === 403 && kind === 'Forbidden') return KIND.CATALOG_GONE;
    if (status === 404 && (kind === 'CatalogNotSelected' || kind === 'UnknownName')) return KIND.CATALOG_GONE;
    if (status === 404 && kind === 'NotFound') return KIND.CONCEPT_GONE;
    return KIND.FAILED;
}

// GET /api/v1<path>: { data, offline, cachedAt }, or a ReadError.
// `offline` is true when the worker answered from its cache because the
// network failed; `cachedAt` is when that copy was stored.
export async function read(path) {
    let response;
    try {
        response = await fetch(`/api/v1${path}`, { headers: { Accept: 'application/json' } });
    } catch {
        throw new ReadError(KIND.UNAVAILABLE, 'Not available offline.');
    }
    const body = await response.json().catch(() => null);
    if (!response.ok) {
        throw new ReadError(classify(response.status, body), body?.message ?? `HTTP ${response.status}`, body);
    }
    return {
        data: body,
        offline: response.headers.has('X-Read-Offline'),
        cachedAt: response.headers.get('X-Read-Cached-At'),
    };
}

// Every page of a listing (`next` cursors, as the console's api.pages):
// { data: <all items of `field`>, offline, cachedAt } of the first page.
export async function readAll(path, field) {
    const first = await read(path);
    const items = [...(first.data[field] ?? [])];
    let cursor = first.data.next ?? null;
    while (cursor !== null) {
        const page = await read(`${path}${path.includes('?') ? '&' : '?'}after=${enc(cursor)}`);
        items.push(...(page.data[field] ?? []));
        const next = page.data.next ?? null;
        cursor = next === cursor ? null : next;
    }
    return { ...first, data: items };
}

// Ends the session (DELETE /api/v1/session with its CSRF token).
// Returns whether the server confirmed it; throws a ReadError when offline.
export async function signOut(csrf) {
    try {
        const response = await fetch('/api/v1/session', { method: 'DELETE', headers: { 'X-OKF-CSRF': csrf } });
        return response.ok || response.status === 401;
    } catch {
        throw new ReadError(KIND.UNAVAILABLE, 'Not available offline.');
    }
}
```

- [ ] **Step 2: Write `src/http/read/js/cache.js`**

```js
// The page's side of the reader's caches (reader design, "Caching"). The
// page and the worker share CacheStorage, so wiping and evicting happen
// here, where the answers are classified; the worker only fills them.
import { enc, encPath } from './api.js';

// The owner record (read-owner) is kept by the worker (sw.js, `admit`);
// the page only deletes it with the rest.
const { content, meta, owner } = self.READ_CACHES;
// How long to wait for the worker to confirm a mark before deleting anyway.
const MARK_WAIT_MS = 1000;
// Without Cache Storage (an unusual private mode) the reader works online
// and keeps nothing.
const CACHING = 'caches' in self;

// Runs `work` against Cache Storage. When storage is missing or refuses
// (denied by browser settings, a full disk), it logs and returns `fallback`
// instead of throwing, so boot, reads and deletion never fail on it.
async function storage(work, fallback = undefined) {
    if (!CACHING) return fallback;
    try {
        return await work();
    } catch (error) {
        console.warn('reader cache unavailable:', error.name);
        return fallback;
    }
}

// Tells the worker that `scope` ('*', an encoded catalog, or a document's
// API path) is invalid from now on, and waits for it to confirm, so the mark is in place before the
// deletion it announces (see sw.js). With no worker controlling the page,
// no answer of the worker's can be on its way, so there is nothing to mark.
function mark(scope) {
    const worker = navigator.serviceWorker?.controller;
    if (!worker) return Promise.resolve();
    return new Promise(resolve => {
        const channel = new MessageChannel();
        const timer = setTimeout(resolve, MARK_WAIT_MS);
        channel.port1.onmessage = () => { clearTimeout(timer); resolve(); };
        worker.postMessage({ type: 'invalidate', scope }, [channel.port2]);
    });
}

// Deletes every cached catalog read and the record of whose they were.
// Deleting needs no free storage, so this works on a full disk too.
export async function wipe() {
    await mark('*');
    await storage(() => Promise.all([content, meta, owner].map(name => caches.delete(name))));
}

// Makes the caches `user`'s: wipes them when they were someone else's.
// Deletes the entries of `cacheNames` whose URL path `matches`.
function evict(cacheNames, matches) {
    return storage(async () => {
        await Promise.all(cacheNames.map(async name => {
            const cache = await caches.open(name);
            const keys = await cache.keys();
            await Promise.all(keys.filter(key => matches(new URL(key.url).pathname)).map(key => cache.delete(key)));
        }));
    });
}

// Deletes everything cached for `catalog`, and keeps answers already on
// their way from putting it back.
export async function evictCatalog(catalog) {
    await mark(enc(catalog));
    const prefix = `/api/v1/catalogs/${enc(catalog)}/`;
    await evict([content, meta], path => path.startsWith(prefix));
}

// Deletes the cached copies (short and full) of one document, and keeps
// answers already on their way from putting it back: `kind` is 'concepts'
// or 'documents', as in its API URL.
export async function evictDocument(catalog, kind, path) {
    const exact = `/api/v1/catalogs/${enc(catalog)}/${kind}/${encPath(path)}`;
    await mark(exact);
    await evict([content], url => url === exact);
}

// The concepts and documents of `catalog` readable offline, newest first,
// one per path: [{ path, title, cachedAt }].
export function offlineDocuments(catalog) {
    return storage(async () => {
        const cache = await caches.open(content);
        const keys = await cache.keys();
        const prefix = `/api/v1/catalogs/${enc(catalog)}/`;
        const seen = new Map();
        // Newest last in the cache, so walk backwards and keep the first of each path.
        for (const key of keys.reverse()) {
            if (!new URL(key.url).pathname.startsWith(prefix)) continue;
            const response = await cache.match(key);
            const body = await response.json().catch(() => null);
            if (!body?.path || seen.has(body.path)) continue;
            seen.set(body.path, { path: body.path, title: body.title ?? null, cachedAt: response.headers.get('X-Read-Cached-At') });
        }
        return [...seen.values()];
    }, []);
}
```

- [ ] **Step 3: Write `src/http/read/js/route.js`**

```js
// The reader's routes, in the URL hash, every segment percent-encoded:
//   #/                                    home
//   #/c[?gone=<catalog>]                  catalog picker
//   #/t/<catalog>[/<folder>...]           a folder of the tree
//   #/s/<catalog>[?q=<query>]             search
//   #/v/<catalog>/<path>.md[/<section>]   a concept, optionally at a heading
//   #/d/<catalog>/<path>                  a log.md or AGENTS.md (read as a document)
//   #/me                                  account
const dec = decodeURIComponent;
const enc = encodeURIComponent;

function segments(path) {
    return String(path ?? '').replace(/^\/+/, '').split('/').filter(Boolean);
}

export function parse(hash) {
    const [body, query = ''] = hash.replace(/^#/, '').split('?');
    const [screen = '', catalog = null, ...rest] = body.split('/').filter(Boolean).map(dec);
    const params = new URLSearchParams(query);
    switch (screen) {
        case 'c': return { screen: 'catalogs', gone: params.get('gone') };
        case 't': return { screen: 'tree', catalog, folder: rest.join('/') };
        case 's': return { screen: 'search', catalog, query: params.get('q') ?? '' };
        case 'v': {
            const end = rest.findIndex(segment => segment.endsWith('.md'));
            if (end < 0) return { screen: 'tree', catalog, folder: rest.join('/') };
            return { screen: 'concept', catalog, path: rest.slice(0, end + 1).join('/'), section: rest[end + 1] ?? null };
        }
        case 'd': return { screen: 'document', catalog, path: rest.join('/') };
        case 'me': return { screen: 'me' };
        default: return { screen: 'home' };
    }
}

export const hrefs = Object.freeze({
    home: () => '#/',
    catalogs: (gone = null) => (gone ? `#/c?gone=${enc(gone)}` : '#/c'),
    tree: (catalog, folder = '') => `#/t/${[catalog, ...segments(folder)].map(enc).join('/')}`,
    search: (catalog, query = '') => `#/s/${enc(catalog)}${query ? `?q=${enc(query)}` : ''}`,
    concept: (catalog, path, section = null) =>
        `#/v/${[catalog, ...segments(path), ...(section ? [section] : [])].map(enc).join('/')}`,
    document: (catalog, path) => `#/d/${[catalog, ...segments(path)].map(enc).join('/')}`,
    me: () => '#/me',
});

// The folder holding `path` ('a/b/c.md' -> 'a/b').
export function parentOf(path) {
    return segments(path).slice(0, -1).join('/');
}

// The files the server serves through /documents, not /concepts (as the
// console's browse.js).
export const DOCUMENTS = new Set(['index.md', 'log.md', 'AGENTS.md']);

// Where a document opens: a folder's index.md opens its folder, a log.md or
// AGENTS.md the document view, anything else the concept view. Every link
// the reader makes (recent changes, backlinks, search hits, body links,
// the offline list) goes through here.
export function hrefFor(catalog, path, section = null) {
    const parts = segments(path);
    const name = parts[parts.length - 1];
    if (name === 'index.md') return hrefs.tree(catalog, parts.slice(0, -1).join('/'));
    if (DOCUMENTS.has(name)) return hrefs.document(catalog, parts.join('/'));
    return hrefs.concept(catalog, parts.join('/'), section);
}

// A link src/render.rs wrote ('#/browse/<catalog>/<path>[/<section>]', the
// console's route) as the reader's route, or null for any other link.
export function fromBrowse(href) {
    if (!href.startsWith('#/browse/')) return null;
    const [catalog, ...rest] = href.slice('#/browse/'.length).split('/').map(dec);
    const end = rest.findIndex(segment => segment.endsWith('.md'));
    if (end < 0) return hrefs.tree(catalog, rest.join('/'));
    return hrefFor(catalog, rest.slice(0, end + 1).join('/'), rest[end + 1] ?? null);
}
```

- [ ] **Step 4: Write `src/http/read/js/listing.js`**

```js
// One folder of a catalog's tree (GET /catalogs/{c}/tree's documents:
// [{ path: '/a/b.md', kind, title }]): its subfolders, its index and its
// concepts. Logs and machinery files are left out of the reader.

function label(doc) {
    return doc.title ?? doc.path.split('/').pop();
}

export function folderListing(documents, folder) {
    const prefix = folder ? `/${folder}/` : '/';
    const folders = new Set();
    const concepts = [];
    let index = null;
    for (const doc of documents) {
        if (!doc.path.startsWith(prefix)) continue;
        const rest = doc.path.slice(prefix.length);
        const slash = rest.indexOf('/');
        if (slash >= 0) folders.add(rest.slice(0, slash));
        else if (doc.kind === 'index') index = doc;
        else if (doc.kind === 'concept') concepts.push(doc);
    }
    concepts.sort((a, b) => label(a).localeCompare(label(b)));
    return { folders: [...folders].sort((a, b) => a.localeCompare(b)), index, concepts, label };
}
```

- [ ] **Step 5: Write `src/http/read/js/parts.js`**

```js
// View parts more than one screen uses.
import { h } from '/ui/js/core/dom.js';
import { fromBrowse, hrefs } from './route.js';

// The search box, which opens the search screen for `catalog`.
export function searchForm(catalog, query = '') {
    const input = h('input', { type: 'search', name: 'q', id: 'search-input', placeholder: 'Search', value: query, 'aria-label': 'Search', enterkeyhint: 'search' });
    return h('form', {
        id: 'search-form', role: 'search',
        onsubmit: event => {
            event.preventDefault();
            const text = input.value.trim();
            if (text) location.hash = hrefs.search(catalog, text);
        },
    }, input);
}

// Breadcrumbs from the catalog root to the folder holding `path` (or to
// `path` itself when it is a folder).
export function crumbs(catalog, path, isFolder) {
    const parts = String(path ?? '').split('/').filter(Boolean);
    const folders = isFolder ? parts : parts.slice(0, -1);
    const items = [h('li', {}, h('a', { href: hrefs.tree(catalog) }, 'Contents'))];
    folders.forEach((name, at) => {
        items.push(h('li', {}, h('a', { href: hrefs.tree(catalog, folders.slice(0, at + 1).join('/')) }, name)));
    });
    return h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb' }, h('ol', {}, items));
}

// body_html from src/render.rs, made the reader's: links to the console's
// browse routes go to reader routes, web links open outside the app.
export function renderBody(html) {
    const article = h('article', { class: 'markdown', id: 'body' });
    // The reader's only innerHTML, as in the console: render.rs writes
    // body_html as safe HTML (no raw HTML, no unsafe link schemes).
    article.innerHTML = html;
    for (const link of article.querySelectorAll('a[href]')) {
        const href = link.getAttribute('href');
        const route = fromBrowse(href);
        if (route) link.setAttribute('href', route);
        else if (/^https?:/i.test(href)) {
            link.setAttribute('target', '_blank');
            link.setAttribute('rel', 'noopener');
        }
    }
    return article;
}

// A short notice with an optional action (a link or button).
export function message(text, action = null, id = 'notice') {
    return h('div', { class: 'notice', id }, h('p', {}, text), action);
}
```

- [ ] **Step 6: Check every module is served and the listing grows**

Run: `scripts/test-pg.sh run cargo test --lib http::read::tests::the_shell_listing`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/http/read/js
git commit -m "feat(read): client modules for reads, caches, routes and shared parts

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Boot, sign-in, account and sign-out

**Files:**
- Create: `src/http/read/js/main.js`, `src/http/read/js/screens/me.js`, and minimal `home.js`, `catalogs.js`, `tree.js`, `search.js`, `concept.js` (completed in Tasks 7 to 9)
- Modify: `tests/ui_e2e/harness.rs`, `tests/ui_e2e/main.rs`
- Create: `tests/ui_e2e/read.rs`

- [ ] **Step 1: Add the harness helpers**

In `tests/ui_e2e/harness.rs`, add to `impl Ui`:

```rust
    /// Navigates to the reader at `#<route>` (route like "/v/main/a.md";
    /// "" for its home).
    pub fn open_reader(&self, route: &str) {
        self.page
            .goto(&format!("{}/read/#{route}", self.base), GotoOptions::default())
            .expect("navigate to the reader");
    }

    /// Opens the reader at `#<route>` signed out, signs in on the page it
    /// sends the browser to, and waits until the reader is back and shown.
    pub fn sign_in_reader(&self, route: &str, email: &str, password: &str) {
        self.open_reader(route);
        self.wait_visible("#signin-email");
        self.fill("#signin-email", email);
        self.fill("#signin-password", password);
        self.click("#signin-submit");
        self.wait_visible("#reader");
    }

    /// Waits until the reader's service worker controls the page.
    pub fn wait_worker(&self) {
        self.poll("the reader's service worker", || {
            self.eval(
                "() => navigator.serviceWorker.controller !== null",
                serde_json::json!(null),
            ) == serde_json::json!(true)
        });
    }

    /// Cuts the page and every service worker off the network (DevTools
    /// network emulation on each target) while the returned value lives;
    /// dropping it closes the connection, which ends the emulation.
    pub fn go_offline(&self) -> DevTools {
        let mut devtools = DevTools::connect(&self.browser);
        let targets = devtools.call(None, "Target.getTargets", serde_json::json!({}));
        let ids: Vec<String> = targets["targetInfos"]
            .as_array()
            .expect("targets")
            .iter()
            .filter(|target| target["type"] == "page" || target["type"] == "service_worker")
            .filter_map(|target| target["targetId"].as_str().map(str::to_string))
            .collect();
        for id in ids {
            let attached = devtools.call(
                None,
                "Target.attachToTarget",
                serde_json::json!({ "targetId": id, "flatten": true }),
            );
            let session = attached["sessionId"].as_str().expect("a session").to_string();
            devtools.call(Some(&session), "Network.enable", serde_json::json!({}));
            devtools.call(
                Some(&session),
                "Network.emulateNetworkConditions",
                serde_json::json!({
                    "offline": true,
                    "latency": 0,
                    "downloadThroughput": -1,
                    "uploadThroughput": -1,
                }),
            );
        }
        devtools
    }

    /// Reloads the current page and waits for the reader to show again.
    pub fn reload_reader(&self) {
        self.page.reload(GotoOptions::default()).expect("reload");
        self.wait_visible("#reader");
    }
```

`poll` is private; it is in the same `impl`, so this compiles. In `tests/ui_e2e/main.rs`, add `mod read;` in alphabetical order (after `mod maintenance;`... `mod oidc_mock;`, before `mod recent;`).

- [ ] **Step 2: Write the failing journeys**

Create `tests/ui_e2e/read.rs`:

```rust
//! The mobile reader at `/read/` (reader design): sign-in through the
//! console's session, the catalog picker, the tree, concepts and their
//! links, search, offline reading from the service worker's caches, and
//! what a revoked session, a removed grant or an unlock does to them.

use chrono::Utc;
use okf_mcp::config::Config;
use okf_mcp::store::audit::AuditActor;
use okf_mcp::store::ceiling::{self, NewEntitlement};
use okf_mcp::store::keys::Access;
use okf_mcp::store::user_sessions;
use serde_json::json;

use crate::harness::drive;
use crate::support::server::{files, TestServer};

const PASSWORD: &str = "violet-lantern-orbit-42";
const EMAIL: &str = "reader@example.com";

/// Every cache's entry count, by cache name.
const CACHE_COUNTS: &str = "async () => { const out = {}; \
    for (const name of await caches.keys()) out[name] = (await (await caches.open(name)).keys()).length; \
    return out; }";

/// `main`: a root index, a `systems` folder with its index and two
/// concepts (alpha links to beta's Settings section), plus `other`.
fn main_files() -> okf_mcp::archive::Files {
    files(&[
        ("index.md", "# Main\n\n- [Systems](systems/index.md)\n"),
        ("log.md", "# Log\n"),
        ("systems/index.md", "# Systems\n\n- [Alpha](alpha.md)\n- [Beta](beta.md)\n"),
        (
            "systems/alpha.md",
            "---\ntype: system\ntitle: Alpha\n---\n# Alpha\n\nSee [Beta settings](beta.md#settings).\n\nThe word marmalade.\n",
        ),
        (
            "systems/beta.md",
            "---\ntype: system\ntitle: Beta\n---\n# Beta\n\nIntro.\n\n## Settings\n\nTwo settings.\n",
        ),
    ])
}

/// A server with `main` and `other`, sign-in routes on, and a user
/// `reader` who can read both.
async fn server() -> (TestServer, String) {
    let server = TestServer::start_with(
        vec![
            ("main", main_files()),
            ("other", files(&[("index.md", "# Other\n"), ("notes.md", "---\ntitle: Notes\n---\n# Notes\n")])),
        ],
        |base| Config {
            public_url: Some(base.to_string()),
            ..Config::default()
        },
    )
    .await;
    let id = server.user_with_password("reader", EMAIL, PASSWORD).await;
    for catalog in ["main", "other"] {
        crate::support::server::applied(&server.app, async {
            ceiling::entitle(
                &server.db.pool,
                &NewEntitlement { user: "reader", catalog, access: Access::Read },
                &AuditActor::cli(),
                Utc::now(),
            )
            .await
            .expect("entitle");
        })
        .await;
    }
    (server, id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signing_in_returns_to_the_route_and_signing_out_wipes_the_caches() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_text("#account-user", "reader");
        ui.wait_worker();
        ui.click("#sign-out");
        ui.wait_visible("#signin-email");
        let counts = ui.eval(CACHE_COUNTS, json!(null));
        for name in ["read-content", "read-meta", "read-owner"] {
            assert!(counts.get(name).is_none(), "{name} survived sign-out: {counts}");
        }
    })
    .await;
}
```

- [ ] **Step 3: Run it to see it fail**

Run: `scripts/test-pg.sh run cargo test --test ui_e2e read:: -- --test-threads=2`
Expected: FAIL, timed out waiting for `#signin-email` (no `main.js` yet).

- [ ] **Step 4: Write `src/http/read/js/main.js`**

```js
// The reader's boot and router (reader design). It checks the session,
// makes the caches this user's, registers the worker, then renders the
// screen the hash names. Every ReadError a screen throws comes back here,
// to fail(), the one place that decides what a refusal does.
import { $, h, clear } from '/ui/js/core/dom.js';
import { ago } from '/ui/js/core/format.js';
import { read, ReadError, KIND } from './api.js';
import { wipe, evictCatalog, evictDocument } from './cache.js';
import { parse, hrefs, parentOf } from './route.js';
import { message } from './parts.js';
import * as home from './screens/home.js';
import * as catalogs from './screens/catalogs.js';
import * as tree from './screens/tree.js';
import * as search from './screens/search.js';
import * as concept from './screens/concept.js';
// Not `document`: that name would hide the browser's global in this module.
import * as documentScreen from './screens/document.js';
import * as me from './screens/me.js';

const SCREENS = { home, catalogs, tree, search, concept, document: documentScreen, me };
const NEEDS_CATALOG = new Set(['home']);
const CATALOG_KEY = 'okf.read.catalog';

const state = { session: null, catalog: remembered() };
// Bumped on every navigation, so a late answer never draws over a newer screen.
let generation = 0;

function remembered() {
    try { return localStorage.getItem(CATALOG_KEY); } catch { return null; }
}

// Makes `catalog` the current one (null forgets it).
function choose(catalog) {
    state.catalog = catalog;
    try {
        if (catalog) localStorage.setItem(CATALOG_KEY, catalog);
        else localStorage.removeItem(CATALOG_KEY);
    } catch { /* storage blocked: the choice lasts for this visit only */ }
    tabs();
}

function signInUrl() {
    return `/signin?return_to=${encodeURIComponent(`/read/${location.hash}`)}`;
}

async function signIn() {
    await wipe();
    location.assign(signInUrl());
}

// Shows the offline banner when `result` came from the worker's cache.
function freshness(result) {
    if (!result?.offline) return;
    const banner = $('#offline-banner');
    banner.textContent = `Offline. Synced ${ago(result.cachedAt)}.`;
    banner.hidden = false;
}

function tabs() {
    const current = parse(location.hash).screen;
    const catalog = state.catalog;
    $('#tab-browse').setAttribute('href', catalog ? hrefs.tree(catalog) : hrefs.catalogs());
    $('#tab-search').setAttribute('href', catalog ? hrefs.search(catalog) : hrefs.catalogs());
    $('#bar-catalog').textContent = catalog ?? 'Reader';
    const active = { home: 'tab-home', tree: 'tab-browse', concept: 'tab-browse', document: 'tab-browse', search: 'tab-search', me: 'tab-me' }[current];
    for (const tab of document.querySelectorAll('.tabs a')) {
        if (tab.id === active) tab.setAttribute('aria-current', 'page');
        else tab.removeAttribute('aria-current');
    }
}

// What a refusal changes, whichever screen shows now: a 401 or a lost
// grant is true even when the screen that asked has been left. Run as soon
// as that one request is refused, never after its siblings.
async function effects(route, error) {
    switch (error.kind) {
        case KIND.SIGNED_OUT:
            await signIn();
            return;
        case KIND.CATALOG_GONE:
            if (route.catalog) await evictCatalog(route.catalog);
            if (route.catalog === state.catalog) choose(null);
            return;
        case KIND.CONCEPT_GONE:
            await evictDocument(route.catalog, route.screen === 'document' ? 'documents' : 'concepts', route.path);
            return;
        case KIND.LOCKED:
        case KIND.UNAVAILABLE:
        case KIND.FAILED:
            return;
    }
}

// What a refusal shows, on the screen that asked, while it is still shown.
function draw(view, route, error, live) {
    if (!live()) return;
    switch (error.kind) {
        case KIND.SIGNED_OUT:
            return; // effects() has already left for sign-in.
        case KIND.CATALOG_GONE:
            location.hash = hrefs.catalogs(route.catalog);
            return;
        case KIND.CONCEPT_GONE:
            clear(view).append(message('This concept no longer exists.',
                h('a', { href: hrefs.tree(route.catalog, parentOf(route.path)), id: 'gone-folder' }, 'Open its folder'), 'concept-gone'));
            return;
        case KIND.LOCKED:
            clear(view).append(message(error.message,
                error.body?.error === 'UnlockRequired' ? h('a', { href: '/ui/', id: 'unlock-link' }, 'Unlock in the console') : null, 'locked'));
            return;
        case KIND.UNAVAILABLE:
            clear(view).append(message('Not available offline.',
                h('a', { href: hrefs.home(), id: 'offline-list-link' }, 'See what is available offline'), 'unavailable'));
            return;
        case KIND.FAILED:
            clear(view).append(message(error.message, null, 'failed'));
            return;
    }
}

// The kinds from most to least serious: when several reads of one screen
// are refused, the most serious one alone draws.
const SEVERITY = [KIND.SIGNED_OUT, KIND.CATALOG_GONE, KIND.CONCEPT_GONE, KIND.LOCKED, KIND.FAILED, KIND.UNAVAILABLE];

function worst(errors) {
    return [...errors].sort((a, b) => SEVERITY.indexOf(a.kind) - SEVERITY.indexOf(b.kind))[0];
}

async function show() {
    const parsed = parse(location.hash);
    const mine = ++generation;
    if ((parsed.catalog ?? null) !== null && parsed.catalog !== state.catalog) choose(parsed.catalog);
    // The catalog this screen reads, fixed now: a refusal that answers after
    // the user switched catalogs must act on the one that was asked about.
    const route = { ...parsed, catalog: parsed.catalog ?? state.catalog };
    tabs();
    $('#offline-banner').hidden = true;
    if (NEEDS_CATALOG.has(route.screen) && !route.catalog) {
        location.replace(hrefs.catalogs());
        return;
    }
    const view = h('section', { class: 'screen', id: `screen-${route.screen}` });
    clear($('#reader')).append(view);
    const live = () => mine === generation;
    const ctx = {
        state, route, choose, freshness, live,
        // A single refused read (a screen's, or a button's later): its
        // effects, then its drawing.
        fail: async error => { await effects(route, error); draw(view, route, error, live); },
        // For a screen that makes several reads at once: `settle` runs each
        // read's refusal effects the moment it is refused (marking the
        // error handled), and `drawWorst` draws once all have settled.
        settle: promise => promise.catch(async error => {
            if (error instanceof ReadError) {
                error.handled = true;
                await effects(route, error);
            }
            throw error;
        }),
        drawWorst: errors => draw(view, route, worst(errors), live),
    };
    try {
        await SCREENS[route.screen].render(view, ctx);
    } catch (error) {
        if (!(error instanceof ReadError)) throw error;
        if (error.handled) draw(view, route, error, live);
        else await ctx.fail(error);
    }
}

// How long a first visit waits for the new worker to take control.
const WORKER_WAIT_MS = 5000;

// Registers the worker and resolves once it controls this page, so the
// first screen's reads already go through it and are cached (on a first
// visit, a direct concept link included). It resolves anyway, without a
// worker, when registration fails or takes too long: the reader then works
// online and only offline reading is lost.
function registerWorker() {
    if (!('serviceWorker' in navigator)) return Promise.resolve();
    const workers = navigator.serviceWorker;
    if (workers.controller) {
        workers.register('/read/sw.js', { scope: '/read/' }).catch(() => {});
        return Promise.resolve();
    }
    return new Promise(resolve => {
        const timer = setTimeout(resolve, WORKER_WAIT_MS);
        const done = () => { clearTimeout(timer); resolve(); };
        // sw.js calls skipWaiting and clients.claim, so a first install
        // takes this page over and fires controllerchange.
        workers.addEventListener('controllerchange', done, { once: true });
        workers.register('/read/sw.js', { scope: '/read/' }).catch(error => {
            console.warn('offline reading is unavailable:', error.message);
            done();
        });
    });
}

async function boot() {
    await registerWorker();
    try {
        const { data } = await read('/session');
        state.session = data;
    } catch (error) {
        if (!(error instanceof ReadError)) throw error;
        if (error.kind === KIND.SIGNED_OUT) { await signIn(); return; }
        // Offline (or the server failing): read what is cached; the
        // banner says so on the first cached answer.
    }
    $('#reader').hidden = false;
    window.addEventListener('hashchange', show);
    await show();
}

boot();
```

- [ ] **Step 5: Write `src/http/read/js/screens/me.js`**

```js
// Account: who is signed in, until when, sign-out (which deletes every
// offline copy first, even if the server cannot be reached), and a local
// "Delete offline copies" that works with no connection at all.
import { h } from '/ui/js/core/dom.js';
import { when } from '/ui/js/core/format.js';
import { signOut, ReadError } from '../api.js';
import { wipe } from '../cache.js';
import { message } from '../parts.js';

function deleteOffline(view) {
    const button = h('button', { type: 'button', class: 'secondary', id: 'delete-offline' }, 'Delete offline copies');
    button.addEventListener('click', async () => {
        button.disabled = true;
        await wipe();
        view.append(message('Offline copies are deleted from this device.', null, 'offline-deleted'));
    });
    return button;
}

export async function render(view, ctx) {
    const session = ctx.state.session;
    view.append(h('h1', {}, 'Account'));
    if (!session) {
        view.append(message('Account details and sign-out need a connection.', null, 'account-offline'), deleteOffline(view));
        return;
    }
    const button = h('button', { type: 'button', id: 'sign-out' }, 'Sign out');
    button.addEventListener('click', async () => {
        button.disabled = true;
        await wipe();
        try {
            if (await signOut(session.csrf)) {
                location.assign(`/signin?return_to=${encodeURIComponent('/read/')}`);
                return;
            }
        } catch (error) {
            if (!(error instanceof ReadError)) throw error;
        }
        button.disabled = false;
        view.append(message('The server could not be reached to end the session. Offline copies are deleted.', null, 'sign-out-failed'));
    });
    view.append(
        h('p', {}, 'Signed in as ', h('strong', { id: 'account-user' }, session.user)),
        session.email ? h('p', { class: 'hint' }, session.email) : null,
        h('p', { class: 'hint' }, `This session ends by ${when(session.expires_at)}.`),
        button,
        deleteOffline(view));
}
```

- [ ] **Step 6: Write minimal screens so `main.js` loads**

Each of `home.js`, `catalogs.js`, `tree.js`, `search.js`, `concept.js` and `document.js` in `src/http/read/js/screens/` starts as the following, with its screen name in the text; Tasks 7 to 9 replace each one completely:

```js
// Replaced by its full screen in a later task of the reader plan.
import { h } from '/ui/js/core/dom.js';

export async function render(view) {
    view.append(h('p', {}, 'home'));
}
```

- [ ] **Step 7: Run the journey to see it pass**

Run: `scripts/test-pg.sh run cargo test --test ui_e2e read:: -- --test-threads=2`
Expected: PASS.

- [ ] **Step 8: Gates and commit**

Run: `tools/agentic/gate.sh fmt`, `tools/agentic/gate.sh lint`. Expected: exit 0.

```bash
git add src/http/read/js tests/ui_e2e/harness.rs tests/ui_e2e/main.rs tests/ui_e2e/read.rs
git commit -m "feat(read): boot, session sign-in, account and sign-out

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Catalog picker and tree

**Files:**
- Modify (replace): `src/http/read/js/screens/catalogs.js`, `src/http/read/js/screens/tree.js`
- Modify: `tests/ui_e2e/read.rs`

- [ ] **Step 1: Write the failing journey**

Append to `tests/ui_e2e/read.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_picker_and_the_tree_lead_from_a_catalog_to_its_concepts() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/c", EMAIL, PASSWORD);
        // Two catalogs, so the picker stays.
        ui.wait_visible("[data-catalog=other]");
        ui.click("[data-catalog=main]");
        ui.wait_visible("#screen-home");
        ui.click("#tab-browse");
        // The root folder: its index's body, then its folders.
        ui.wait_text("#body", "Main");
        ui.wait_text("#folders", "systems");
        ui.click("#folders a");
        // systems/: its index body, and its two concepts by title.
        ui.wait_text("#body", "Systems");
        ui.wait_text("#concepts", "Alpha");
        ui.wait_text("#concepts", "Beta");
        assert!(!ui.text("#concepts").contains("log"), "logs are not listed");
        ui.wait_text(".crumbs", "Contents");
    })
    .await;
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `scripts/test-pg.sh run cargo test --test ui_e2e read::the_picker -- --test-threads=2`
Expected: FAIL, timed out waiting for `[data-catalog=other]`.

- [ ] **Step 3: Write `src/http/read/js/screens/catalogs.js`**

```js
// The catalog picker. With no catalog chosen yet and only one reachable,
// it chooses that one and goes home.
import { h } from '/ui/js/core/dom.js';
import { readAll } from '../api.js';
import { hrefs } from '../route.js';
import { message } from '../parts.js';

export async function render(view, ctx) {
    const result = await readAll('/catalogs', 'catalogs');
    if (!ctx.live()) return;
    ctx.freshness(result);
    const found = result.data;
    if (found.length === 1 && !ctx.state.catalog && !ctx.route.gone) {
        ctx.choose(found[0].name);
        location.replace(hrefs.home());
        return;
    }
    view.append(h('h1', {}, 'Catalogs'));
    if (ctx.route.gone) {
        view.append(message(`You can no longer read ${ctx.route.gone}. Its offline copies are deleted.`, null, 'catalog-gone'));
    }
    if (!found.length) {
        view.append(message('Your account reaches no catalog.', null, 'no-catalogs'));
        return;
    }
    view.append(h('ul', { class: 'list', id: 'catalog-list' }, found.map(catalog => h('li', {},
        h('button', {
            type: 'button', class: 'row', dataset: { catalog: catalog.name },
            onclick: () => { ctx.choose(catalog.name); location.hash = hrefs.home(); },
        },
        catalog.label ?? catalog.name,
        catalog.description ? h('span', { class: 'meta' }, catalog.description) : null)))));
}
```

- [ ] **Step 4: Write `src/http/read/js/screens/tree.js`**

```js
// One folder of the tree: its index.md rendered first (when it has one and
// it can be read), then its subfolders and its concepts.
import { h } from '/ui/js/core/dom.js';
import { read, enc, encPath, KIND } from '../api.js';
import { hrefs } from '../route.js';
import { folderListing } from '../listing.js';
import { evictDocument } from '../cache.js';
import { crumbs, renderBody } from '../parts.js';

export async function render(view, ctx) {
    const { catalog, folder } = ctx.route;
    const base = `/catalogs/${enc(catalog)}`;
    const result = await read(`${base}/tree`);
    if (!ctx.live()) return;
    ctx.freshness(result);
    const listing = folderListing(result.data.documents, folder);
    view.append(crumbs(catalog, folder, true), h('h1', {}, folder ? folder.split('/').pop() : 'Contents'));
    if (listing.index) {
        try {
            const overview = await read(`${base}/documents/${encPath(listing.index.path)}`);
            if (!ctx.live()) return;
            ctx.freshness(overview);
            view.append(renderBody(overview.data.body_html));
        } catch (error) {
            // Offline without a copy, or deleted since the tree was read:
            // the lists below still work. A deleted index's copy goes.
            if (error.kind === KIND.CONCEPT_GONE) await evictDocument(catalog, 'documents', listing.index.path);
            else if (error.kind !== KIND.UNAVAILABLE) throw error;
        }
    }
    if (listing.folders.length) {
        view.append(h('h2', {}, 'Folders'), h('ul', { class: 'list', id: 'folders' }, listing.folders.map(name =>
            h('li', {}, h('a', { class: 'folder', href: hrefs.tree(catalog, folder ? `${folder}/${name}` : name) }, name)))));
    }
    if (listing.concepts.length) {
        view.append(h('h2', {}, 'Concepts'), h('ul', { class: 'list', id: 'concepts' }, listing.concepts.map(doc =>
            h('li', {}, h('a', { href: hrefs.concept(catalog, doc.path) }, listing.label(doc))))));
    }
}
```

- [ ] **Step 5: Run it to see it pass**

Run: `scripts/test-pg.sh run cargo test --test ui_e2e read:: -- --test-threads=2`
Expected: PASS (both journeys).

- [ ] **Step 6: Commit**

```bash
git add src/http/read/js/screens/catalogs.js src/http/read/js/screens/tree.js tests/ui_e2e/read.rs
git commit -m "feat(read): catalog picker and folder tree

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Concept view

**Files:**
- Modify (replace): `src/http/read/js/screens/concept.js`
- Modify: `tests/ui_e2e/read.rs`

- [ ] **Step 1: Write the failing journey**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concept_shows_its_body_follows_links_and_lists_backlinks() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/v/main/systems/alpha.md", EMAIL, PASSWORD);
        ui.wait_text("#concept-title", "Alpha");
        ui.wait_text("#body", "marmalade");
        ui.wait_text(".crumbs", "systems");
        // render.rs's console link becomes a reader route, at the section.
        let href = ui.eval(
            "() => document.querySelector('#body a').getAttribute('href')",
            json!(null),
        );
        assert_eq!(href, json!("#/v/main/systems/beta.md/settings"));
        ui.click("#body a");
        ui.wait_text("#concept-title", "Beta");
        ui.wait_text("#outline", "Settings");
        // Beta is linked from alpha and from the systems index.
        ui.wait_text("#backlinks", "Alpha");
    })
    .await;
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `scripts/test-pg.sh run cargo test --test ui_e2e read::a_concept -- --test-threads=2`
Expected: FAIL, timed out waiting for `#concept-title`.

- [ ] **Step 3: Write `src/http/read/js/screens/concept.js`**

```js
// A concept: breadcrumbs, title, outline, body (or, when the server
// shortened it, a button for the whole body) and what links to it.
import { h } from '/ui/js/core/dom.js';
import { read, enc, encPath, KIND, ReadError } from '../api.js';
import { hrefs, hrefFor } from '../route.js';
import { crumbs, renderBody, message } from '../parts.js';

// The links read, or null when offline with no copy: the concept still shows.
async function backlinks(base, path) {
    try {
        return await read(`${base}/links/${encPath(path)}`);
    } catch (error) {
        if (error.kind === KIND.UNAVAILABLE) return null;
        throw error;
    }
}

function outline(catalog, path, headings) {
    const below = (headings ?? []).filter(heading => heading.level >= 2);
    if (!below.length) return null;
    return h('details', { class: 'outline', id: 'outline' }, h('summary', {}, 'Outline'),
        h('ul', {}, below.map(heading => h('li', { class: `level-${heading.level}` },
            h('a', { href: hrefs.concept(catalog, path, heading.slug) }, heading.heading)))));
}

function scrollTo(section) {
    if (!section) return;
    requestAnimationFrame(() => document.querySelector(`[data-slug="${CSS.escape(section)}"]`)?.scrollIntoView({ block: 'start' }));
}

export async function render(view, ctx) {
    const { catalog, path, section } = ctx.route;
    const base = `/catalogs/${enc(catalog)}`;
    const conceptUrl = `${base}/concepts/${encPath(path)}`;
    // A section route asks for the whole body, as the console does: over
    // concept_outline_chars the short answer has no body, so the heading to
    // scroll to would be missing (every navigation renders afresh, an
    // outline tap after "Show the whole concept" included).
    const firstUrl = section ? `${conceptUrl}?full=true` : conceptUrl;
    // Each read's refusal is acted on the moment it comes (ctx.settle: a
    // 401 wipes and signs in without waiting for the other read); what to
    // draw is decided once both have settled.
    const settled = await Promise.allSettled([ctx.settle(read(firstUrl)), ctx.settle(backlinks(base, path))]);
    const refused = settled.filter(outcome => outcome.status === 'rejected').map(outcome => outcome.reason);
    const unexpected = refused.find(reason => !(reason instanceof ReadError));
    if (unexpected) throw unexpected;
    if (refused.length) {
        ctx.drawWorst(refused);
        return;
    }
    const [result, links] = settled.map(outcome => outcome.value);
    if (!ctx.live()) return;
    ctx.freshness(result);
    ctx.freshness(links);
    const concept = result.data;
    const body = h('div', { id: 'body-slot' });
    view.append(
        crumbs(catalog, path, false),
        h('h1', { id: 'concept-title' }, concept.title ?? path),
        outline(catalog, path, concept.outline),
        body);
    if (concept.body_html) {
        body.append(renderBody(concept.body_html));
        scrollTo(section);
    } else {
        const button = h('button', { type: 'button', id: 'show-full' }, 'Show the whole concept');
        button.addEventListener('click', async () => {
            button.disabled = true;
            try {
                const full = await read(`${conceptUrl}?full=true`);
                ctx.freshness(full);
                body.replaceChildren(renderBody(full.data.body_html));
                scrollTo(section);
            } catch (error) {
                if (!(error instanceof ReadError)) throw error;
                button.disabled = false;
                if (error.kind === KIND.UNAVAILABLE) {
                    body.append(message('The whole concept is not available offline.', null, 'full-failed'));
                    return;
                }
                // A 401 or a lost grant here must wipe or evict like any other read.
                await ctx.fail(error);
            }
        });
        body.append(message('This concept is long; only its outline was loaded.', button, 'shortened'));
    }
    view.append(h('h2', {}, 'Linked from'));
    if (!links) {
        view.append(h('p', { class: 'hint', id: 'backlinks' }, 'Links are not available offline.'));
    } else if (!links.data.backlinks.length) {
        view.append(h('p', { class: 'hint', id: 'backlinks' }, 'Nothing links here.'));
    } else {
        view.append(h('ul', { class: 'list', id: 'backlinks' }, links.data.backlinks.map(link =>
            h('li', {}, h('a', { href: hrefFor(catalog, link.path) }, link.title ?? link.path)))));
    }
}
```

- [ ] **Step 4: Write `src/http/read/js/screens/document.js`**

A `log.md` or `AGENTS.md` (the server reads these through `/documents`, not `/concepts`), so links to them from recent changes, backlinks and bodies open something real:

```js
// A log.md or AGENTS.md: breadcrumbs, its name and its rendered text.
import { h } from '/ui/js/core/dom.js';
import { read, enc, encPath } from '../api.js';
import { crumbs, renderBody } from '../parts.js';

export async function render(view, ctx) {
    const { catalog, path } = ctx.route;
    const result = await read(`/catalogs/${enc(catalog)}/documents/${encPath(path)}`);
    if (!ctx.live()) return;
    ctx.freshness(result);
    view.append(crumbs(catalog, path, false), h('h1', { id: 'document-title' }, path.split('/').pop()),
        renderBody(result.data.body_html));
}
```

And a journey for the route itself in `tests/ui_e2e/read.rs` (Task 9 adds the one that reaches it from recent changes, once Home exists):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_log_opens_as_a_document() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/d/main/log.md", EMAIL, PASSWORD);
        ui.wait_text("#document-title", "log.md");
        ui.wait_text("#body", "Log");
        assert!(!ui.present("#concept-gone") && !ui.present("#failed"));
    })
    .await;
}
```

- [ ] **Step 5: Run it to see it pass**

Run: `scripts/test-pg.sh run cargo test --test ui_e2e read:: -- --test-threads=2`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/http/read/js/screens/concept.js src/http/read/js/screens/document.js src/http/read/js/route.js tests/ui_e2e/read.rs
git commit -m "feat(read): concept view with outline, reader links and backlinks

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: Home and search

**Files:**
- Modify (replace): `src/http/read/js/screens/home.js`, `src/http/read/js/screens/search.js`
- Modify: `tests/ui_e2e/read.rs`

- [ ] **Step 1: Write the failing journey**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn home_lists_recent_changes_and_search_finds_a_concept() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/c", EMAIL, PASSWORD);
        ui.click("[data-catalog=main]");
        ui.wait_text("#recent", "systems/alpha.md");
        ui.fill("#search-input", "marmalade");
        ui.eval("() => { document.querySelector('#search-form').requestSubmit(); return true; }", json!(null));
        ui.wait_text("#hits", "Alpha");
        ui.click("#hits a");
        ui.wait_text("#concept-title", "Alpha");
    })
    .await;
}
```

And one that checks recent changes links a `log.md` to the document view (the fixture imports `log.md`, so recent changes lists it):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_log_in_recent_changes_opens_as_a_document() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/c", EMAIL, PASSWORD);
        ui.click("[data-catalog=main]");
        ui.wait_visible("#recent");
        let href = ui.eval(
            "() => [...document.querySelectorAll('#recent a')].map(a => a.getAttribute('href')).find(h => h.endsWith('log.md'))",
            json!(null),
        );
        assert_eq!(href, json!("#/d/main/log.md"));
        ui.click("#recent a[href='#/d/main/log.md']");
        ui.wait_text("#document-title", "log.md");
    })
    .await;
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `scripts/test-pg.sh run cargo test --test ui_e2e read::home_lists -- --test-threads=2`
Expected: FAIL, timed out waiting for `#recent`.

- [ ] **Step 3: Write `src/http/read/js/screens/home.js`**

```js
// Home: the search box, recent changes in the current catalog, and, when
// the server cannot be reached, what this device can read offline. On an
// iPhone's browser (not yet installed) it says how to install.
import { h } from '/ui/js/core/dom.js';
import { ago } from '/ui/js/core/format.js';
import { read, enc, KIND } from '../api.js';
import { offlineDocuments } from '../cache.js';
import { hrefFor } from '../route.js';
import { searchForm, message } from '../parts.js';

const HINT_KEY = 'okf.read.install-hint';

function installHint() {
    const ios = /iPad|iPhone|iPod/.test(navigator.userAgent)
        || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);
    let dismissed = false;
    try { dismissed = localStorage.getItem(HINT_KEY) === 'dismissed'; } catch { /* storage blocked */ }
    if (!ios || navigator.standalone || dismissed) return null;
    const close = h('button', { type: 'button', class: 'secondary outline' }, 'Got it');
    const hint = message('To install: tap Share, then Add to Home Screen.', close, 'install-hint');
    close.addEventListener('click', () => {
        try { localStorage.setItem(HINT_KEY, 'dismissed'); } catch { /* storage blocked */ }
        hint.remove();
    });
    return hint;
}

async function offlineList(view, catalog) {
    const items = await offlineDocuments(catalog);
    view.append(h('h2', {}, 'Available offline'));
    if (!items.length) {
        view.append(h('p', { class: 'hint', id: 'offline-list' }, 'Nothing yet. Concepts you open are kept for reading offline.'));
        return;
    }
    view.append(h('ul', { class: 'list', id: 'offline-list' }, items.map(item => h('li', {},
        h('a', { href: hrefFor(catalog, item.path) }, item.title ?? item.path,
            h('span', { class: 'meta' }, `Synced ${ago(item.cachedAt)}`))))));
}

export async function render(view, ctx) {
    const catalog = ctx.route.catalog;
    view.append(installHint(), searchForm(catalog));
    let result;
    try {
        result = await read(`/catalogs/${enc(catalog)}/recent-changes?limit=20`);
    } catch (error) {
        if (error.kind !== KIND.UNAVAILABLE) throw error;
        await offlineList(view, catalog);
        return;
    }
    if (!ctx.live()) return;
    ctx.freshness(result);
    view.append(h('h2', {}, 'Recent changes'), h('ul', { class: 'list', id: 'recent' }, result.data.changes.map(change =>
        h('li', {}, change.op === 'delete'
            ? h('span', {}, change.path, h('span', { class: 'meta' }, `Deleted ${ago(change.at)}`))
            : h('a', { href: hrefFor(catalog, change.path) }, change.path,
                h('span', { class: 'meta' }, `${change.message || change.op} - ${change.actor_label ?? change.actor}, ${ago(change.at)}`))))));
    if (result.offline) await offlineList(view, catalog);
}
```

- [ ] **Step 4: Write `src/http/read/js/screens/search.js`**

```js
// Search in the current catalog. It needs the server: offline it says so
// and points at what is available offline.
import { h } from '/ui/js/core/dom.js';
import { count } from '/ui/js/core/format.js';
import { read, enc, KIND } from '../api.js';
import { hrefs, hrefFor } from '../route.js';
import { searchForm, message } from '../parts.js';

export async function render(view, ctx) {
    const { catalog, query } = ctx.route;
    view.append(searchForm(catalog, query));
    if (!query) return;
    let result;
    try {
        result = await read(`/search?q=${enc(query)}&catalogs=${enc(catalog)}&limit=50`);
    } catch (error) {
        if (error.kind !== KIND.UNAVAILABLE) throw error;
        view.append(message('Search needs a connection.',
            h('a', { href: hrefs.home(), id: 'offline-list-link' }, 'See what is available offline'), 'search-offline'));
        return;
    }
    if (!ctx.live()) return;
    const hits = result.data.hits;
    view.append(h('p', { class: 'hint', id: 'search-count' }, count(hits.length, 'hit')),
        h('ul', { class: 'list', id: 'hits' }, hits.map(hit => h('li', {},
            h('a', { href: hrefFor(hit.catalog, hit.path) }, hit.title ?? hit.path,
                hit.snippet ? h('span', { class: 'meta' }, hit.snippet) : null)))));
}
```

- [ ] **Step 5: Run it to see it pass**

Run: `scripts/test-pg.sh run cargo test --test ui_e2e read:: -- --test-threads=2`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/http/read/js/screens/home.js src/http/read/js/screens/search.js tests/ui_e2e/read.rs
git commit -m "feat(read): home with recent changes and offline list, and search

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: Offline journeys (cache, cap, shell upgrade)

**Files:**
- Modify: `tests/ui_e2e/read.rs`

These journeys exercise `sw.js` and `cache.js` from Tasks 3 and 5. If one fails, fix the module, not the journey.

- [ ] **Step 1: Write the journeys**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn viewed_concepts_are_readable_offline_and_unviewed_ones_say_so() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/c", EMAIL, PASSWORD);
        ui.click("[data-catalog=main]");
        ui.wait_visible("#recent");
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_text("#concept-title", "Beta");
        let offline = ui.go_offline();
        ui.reload_reader();
        ui.wait_text("#concept-title", "Beta");
        ui.wait_text("#offline-banner", "Offline. Synced");
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        // Never opened: other's notes.
        ui.open_reader("/v/other/notes.md");
        ui.wait_visible("#unavailable");
        // Search needs the network.
        ui.open_reader("/s/main?q=marmalade");
        ui.wait_visible("#search-offline");
        // Home lists what can be read.
        ui.open_reader("/");
        ui.wait_text("#offline-list", "Alpha");
        drop(offline);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_concept_a_first_visit_opens_is_readable_offline() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        // A direct link on a first visit, read once, never revisited online.
        ui.sign_in_reader("/v/main/systems/alpha.md", EMAIL, PASSWORD);
        ui.wait_text("#body", "marmalade");
        let offline = ui.go_offline();
        ui.reload_reader();
        ui.wait_text("#body", "marmalade");
        ui.wait_text("#offline-banner", "Offline. Synced");
        drop(offline);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_content_cache_keeps_the_most_recently_viewed_two_hundred() {
    let notes: Vec<(String, String)> = (0..201)
        .map(|n| (format!("notes/n{n:03}.md"), format!("---\ntitle: N{n}\n---\n# N{n}\n")))
        .collect();
    let mut entries: Vec<(&str, &str)> = vec![("index.md", "# Many\n")];
    entries.extend(notes.iter().map(|(path, text)| (path.as_str(), text.as_str())));
    let server = TestServer::start_with(vec![("many", files(&entries))], |base| Config {
        public_url: Some(base.to_string()),
        ..Config::default()
    })
    .await;
    server.user_with_password("reader", EMAIL, PASSWORD).await;
    crate::support::server::applied(&server.app, async {
        ceiling::entitle(
            &server.db.pool,
            &NewEntitlement { user: "reader", catalog: "many", access: Access::Read },
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("entitle");
    })
    .await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        // Read n000 and n001 first, then n001 again after 199 others, so
        // n000 is the least recently viewed when the 201st arrives.
        let fetched = ui.eval(
            "async () => { const get = n => fetch(`/api/v1/catalogs/many/concepts/notes/n${String(n).padStart(3, '0')}.md`); \
             await get(0); await get(1); for (let n = 2; n <= 199; n++) await get(n); await get(1); await get(200); \
             const cache = await caches.open('read-content'); const keys = (await cache.keys()).map(k => new URL(k.url).pathname); \
             return { size: keys.length, has0: keys.some(p => p.endsWith('n000.md')), has1: keys.some(p => p.endsWith('n001.md')) }; }",
            json!(null),
        );
        assert_eq!(fetched, json!({ "size": 200, "has0": false, "has1": true }));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_worker_drops_shell_caches_of_other_versions() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        // Before the worker exists: a shell cache left by an older build.
        ui.page
            .goto(&format!("{}/read/shell.json", ui.base), rustwright::GotoOptions::default())
            .expect("open the origin");
        ui.eval("async () => { await caches.open('read-shell-0000000000000000'); return true; }", json!(null));
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        let names = ui.eval("async () => await caches.keys()", json!(null));
        let names: Vec<&str> = names.as_array().expect("names").iter().filter_map(|n| n.as_str()).collect();
        assert!(!names.contains(&"read-shell-0000000000000000"), "{names:?}");
        assert_eq!(names.iter().filter(|n| n.starts_with("read-shell-")).count(), 1, "{names:?}");
    })
    .await;
}
```

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_copies_can_be_deleted_without_a_connection() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        let offline = ui.go_offline();
        ui.open_reader("/me");
        ui.reload_reader();
        ui.wait_visible("#account-offline");
        ui.click("#delete-offline");
        ui.wait_visible("#offline-deleted");
        let counts = ui.eval(CACHE_COUNTS, json!(null));
        assert!(counts.get("read-content").is_none(), "{counts}");
        drop(offline);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_cache_still_shows_what_the_server_sent() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        // No room left: every cache write from now on fails.
        let full = ui.limit_storage();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        drop(full);
    })
    .await;
}
```

Add to `impl Ui` in `tests/ui_e2e/harness.rs`:

```rust
    /// Sets this origin's storage quota to what it uses now (DevTools'
    /// `Storage.overrideQuotaForOrigin`), so every further cache write is
    /// refused with QuotaExceededError while the returned value lives.
    pub fn limit_storage(&self) -> DevTools {
        let used = self
            .eval("async () => (await navigator.storage.estimate()).usage", serde_json::json!(null))
            .as_f64()
            .expect("storage usage");
        let mut devtools = DevTools::connect(&self.browser);
        let attached = devtools.call(
            None,
            "Target.attachToTarget",
            serde_json::json!({ "targetId": self.page.target_id(), "flatten": true }),
        );
        let session = attached["sessionId"].as_str().expect("a session").to_string();
        devtools.call(
            Some(&session),
            "Storage.overrideQuotaForOrigin",
            serde_json::json!({ "origin": self.base, "quotaSize": used }),
        );
        devtools
    }
```

Two more for refused writes and for the short and full forms of a concept:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_storage_full_the_reader_boots_and_still_deletes_its_copies() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        // The owner record goes too, so the worker must write it again on
        // the next answer, and cannot.
        ui.eval("async () => { await caches.delete('read-owner'); return true; }", json!(null));
        let full = ui.limit_storage();
        ui.open_reader("/me");
        ui.reload_reader();
        ui.wait_visible("#account-user");
        ui.click("#delete-offline");
        ui.wait_visible("#offline-deleted");
        let counts = ui.eval(CACHE_COUNTS, json!(null));
        assert!(counts.get("read-content").is_none(), "{counts}");
        ui.click("#sign-out");
        ui.wait_visible("#signin-email");
        drop(full);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_a_concept_opens_from_either_its_short_or_its_full_copy() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        // Beta opened plainly (its whole body fits): cached without ?full.
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_text("#body", "Two settings");
        // Alpha opened only through a section link: cached with ?full=true.
        ui.open_reader("/v/main/systems/alpha.md/alpha");
        ui.wait_text("#body", "marmalade");
        let offline = ui.go_offline();
        // Beta's outline link asks for ?full=true: the plain copy answers.
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_text("#concept-title", "Beta");
        ui.click("#outline summary");
        ui.click("#outline a");
        ui.wait_text("#body", "Two settings");
        // Alpha opened plainly: the full copy answers.
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        drop(offline);
    })
    .await;
}
```

And one where storage refuses even to open (browser settings can deny it):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_cache_storage_denied_the_reader_still_reads_online() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        let denied = ui.deny_cache_storage();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.reload_reader();
        ui.wait_text("#body", "marmalade");
        ui.open_reader("/");
        ui.wait_visible("#recent");
        ui.open_reader("/me");
        ui.click("#delete-offline");
        ui.wait_visible("#offline-deleted");
        drop(denied);
    })
    .await;
}
```

Add to `impl Ui` in `tests/ui_e2e/harness.rs`:

```rust
    /// Makes every Cache Storage call reject with SecurityError, in the
    /// page (now and on every later load) and in the reader's service
    /// worker, while the returned value lives.
    pub fn deny_cache_storage(&self) -> DevTools {
        const DENY: &str = "for (const name of ['open', 'match', 'keys', 'delete', 'has']) { \
            CacheStorage.prototype[name] = function () { \
                return Promise.reject(new DOMException('denied', 'SecurityError')); }; }";
        let mut devtools = DevTools::connect(&self.browser);
        let targets = devtools.call(None, "Target.getTargets", serde_json::json!({}));
        let infos: Vec<(String, String)> = targets["targetInfos"]
            .as_array()
            .expect("targets")
            .iter()
            .filter_map(|target| Some((target["type"].as_str()?.to_string(), target["targetId"].as_str()?.to_string())))
            .filter(|(kind, _)| kind == "page" || kind == "service_worker")
            .collect();
        for (kind, id) in infos {
            let attached = devtools.call(
                None,
                "Target.attachToTarget",
                serde_json::json!({ "targetId": id, "flatten": true }),
            );
            let session = attached["sessionId"].as_str().expect("a session").to_string();
            if kind == "page" {
                devtools.call(
                    Some(&session),
                    "Page.addScriptToEvaluateOnNewDocument",
                    serde_json::json!({ "source": DENY }),
                );
            }
            devtools.call(Some(&session), "Runtime.evaluate", serde_json::json!({ "expression": DENY }));
        }
        devtools
    }
```

(The worker can be stopped when idle and restarted without the override; the journey runs well inside that window. If it proves flaky, call `deny_cache_storage` again right before the reload.)

If this Chromium does not refuse writes under the override (the concept then renders either way, so the journey proves nothing), check by evaluating a `caches.open('x').then(c => c.put('/x', new Response('y'.repeat(1e6))))` in the page under the override; it must reject.

- [ ] **Step 2: Run them**

Run: `scripts/test-pg.sh run cargo test --test ui_e2e read:: -- --test-threads=2`
Expected: PASS. If `go_offline` does not stop the worker's fetches (the concept loads fresh rather than with the banner), check that `Target.getTargets` lists the `service_worker` target; attach with `Target.setAutoAttach` on the browser session as a fallback.

- [ ] **Step 3: Commit**

```bash
git add tests/ui_e2e/read.rs tests/ui_e2e/harness.rs src/http/read
git commit -m "test(read): offline reading, the 200-concept cap and shell upgrades

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: Revocation journeys (session, grant, unlock)

**Files:**
- Modify: `tests/ui_e2e/read.rs`

- [ ] **Step 1: Write the journeys**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signing_in_as_someone_else_in_another_tab_never_mixes_their_copies() {
    let (server, _) = server().await;
    server.user_with_password("second", "second@example.com", PASSWORD).await;
    crate::support::server::applied(&server.app, async {
        ceiling::entitle(
            &server.db.pool,
            &NewEntitlement { user: "second", catalog: "main", access: Access::Read },
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("entitle");
    })
    .await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        let first = ui.eval("async () => (await fetch('/api/v1/session')).headers.get('x-okf-account')", json!(null));
        // Another tab signs in as `second`; this reader tab stays open.
        ui.in_second_tab(|tab| {
            tab.goto(&format!("{}/signin", ui.base), rustwright::GotoOptions::default()).expect("sign-in page");
            tab.fill("#signin-email", "second@example.com", rustwright::ActionOptions::default()).expect("email");
            tab.fill("#signin-password", PASSWORD, rustwright::ActionOptions::default()).expect("password");
            tab.click("#signin-submit", rustwright::ActionOptions::default()).expect("submit");
        });
        let second = ui.eval("async () => (await fetch('/api/v1/session')).headers.get('x-okf-account')", json!(null));
        assert_ne!(first, second, "the tag names the account");
        // The open tab reads beta, now as `second`: the worker wipes the
        // first account's copies before keeping this one.
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_text("#concept-title", "Beta");
        let paths = ui.eval(
            "async () => (await (await caches.open('read-content')).keys()).map(k => new URL(k.url).pathname)",
            json!(null),
        );
        let paths: Vec<&str> = paths.as_array().expect("paths").iter().filter_map(|p| p.as_str()).collect();
        assert!(paths.iter().all(|p| !p.ends_with("/alpha.md")), "{paths:?}");
        assert!(paths.iter().any(|p| p.ends_with("/beta.md")), "{paths:?}");
        let recorded = ui.eval(
            "async () => (await (await (await caches.open('read-owner')).match('/read/__owner')).json()).tag",
            json!(null),
        );
        assert_eq!(recorded, second);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shell_from_another_build_is_not_installed() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        // As if a deploy landed between the worker's requests: every new
        // worker gets a read.css that is not the one its listing names.
        let tamper = ui.tamper_new_workers("*/read/read.css", "/* another build */");
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.poll_eval(
            "async () => { const r = await navigator.serviceWorker.getRegistration('/read/'); \
             return !!r && !r.installing; }",
        );
        let installed = ui.eval(
            "async () => ({ controlled: navigator.serviceWorker.controller !== null, \
             shells: (await caches.keys()).filter(n => n.startsWith('read-shell-')).length })",
            json!(null),
        );
        assert_eq!(installed, json!({ "controlled": false, "shells": 0 }));
        // The reader still works online.
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        // The next build's files are consistent again: the install goes through.
        drop(tamper);
        ui.reload_reader();
        ui.wait_worker();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_session_wipes_the_caches_on_the_next_answer() {
    let (server, user_id) = server().await;
    let runtime = tokio::runtime::Handle::current();
    let pool = server.db.pool.clone();
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/v/main/systems/alpha.md", EMAIL, PASSWORD);
        ui.wait_text("#body", "marmalade");
        ui.wait_worker();
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_text("#concept-title", "Beta");
        runtime.block_on(async {
            user_sessions::revoke_all(&pool, &user_id, None).await.expect("revoke");
        });
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_visible("#signin-email");
        let counts = ui.eval(CACHE_COUNTS, json!(null));
        assert!(counts.get("read-content").is_none(), "{counts}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_grant_evicts_that_catalog_only() {
    let (server, _) = server().await;
    let runtime = tokio::runtime::Handle::current();
    let (pool, app) = (server.db.pool.clone(), std::sync::Arc::clone(&server.app));
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/v/other/notes.md", EMAIL, PASSWORD);
        ui.wait_text("#concept-title", "Notes");
        ui.wait_worker();
        // Again, now that the worker controls the page and caches reads.
        ui.open_reader("/v/other/notes.md");
        ui.wait_text("#concept-title", "Notes");
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        runtime.block_on(crate::support::server::applied(&app, async {
            ceiling::unentitle(&pool, "reader", "main", &AuditActor::cli(), Utc::now())
                .await
                .expect("unentitle");
        }));
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_visible("#catalog-gone");
        let paths = ui.eval(
            "async () => (await (await caches.open('read-content')).keys()).map(k => new URL(k.url).pathname)",
            json!(null),
        );
        let paths: Vec<&str> = paths.as_array().expect("paths").iter().filter_map(|p| p.as_str()).collect();
        assert!(paths.iter().all(|p| !p.contains("/catalogs/main/")), "{paths:?}");
        assert!(paths.iter().any(|p| p.contains("/catalogs/other/")), "{paths:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unlock_refusal_keeps_the_caches_and_points_at_the_console() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/v/main/systems/alpha.md", EMAIL, PASSWORD);
        ui.wait_text("#body", "marmalade");
        ui.wait_worker();
        // Again, now that the worker controls the page and caches reads.
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        // The next concept read is refused as the server refuses a catalog
        // that needs an unlock (the harness answers it in the page).
        ui.fail_next_kind("GET", "/concepts/systems/beta.md", 403, "UnlockRequired", "This catalog needs an unlock.");
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_visible("#locked");
        let href = ui.eval("() => document.querySelector('#unlock-link')?.getAttribute('href')", json!(null));
        assert_eq!(href, json!("/ui/"));
        let counts = ui.eval(CACHE_COUNTS, json!(null));
        assert!(counts["read-content"].as_u64().unwrap_or(0) >= 1, "{counts}");
    })
    .await;
}
```

The harness's `fail_next` answers with `{ error: 'test', message }`, and this journey needs the kind `UnlockRequired`. In `tests/ui_e2e/harness.rs`, change the `FAIL_NEXT` script's body to `JSON.stringify({ error: arg.kind ?? 'test', message: arg.message })` and add to `impl Ui`:

```rust
    /// As [`Ui::fail_next`], with the body's `error` kind set to `kind`.
    pub fn fail_next_kind(&self, method: &str, suffix: &str, status: u16, kind: &str, message: &str) {
        let armed = self.eval(
            FAIL_NEXT,
            serde_json::json!({ "method": method, "suffix": suffix, "status": status, "kind": kind, "message": message }),
        );
        assert_eq!(armed, serde_json::json!(true));
    }
```

Two more journeys, for refusals that arrive after the screen that asked has gone, and for a read a button makes:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_grant_answered_after_navigating_away_still_evicts() {
    let (server, _) = server().await;
    let runtime = tokio::runtime::Handle::current();
    let (pool, app) = (server.db.pool.clone(), std::sync::Arc::clone(&server.app));
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/v/main/systems/alpha.md", EMAIL, PASSWORD);
        ui.wait_text("#body", "marmalade");
        ui.wait_worker();
        // Again, now that the worker controls the page and caches reads.
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        // Beta's concept read waits in the page; its links read goes through.
        ui.hold_next("GET", "/concepts/systems/beta.md");
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_visible("#screen-concept");
        // Leave for a screen that reads nothing, then lose the grant.
        ui.open_reader("/me");
        ui.wait_visible("#account-user");
        runtime.block_on(crate::support::server::applied(&app, async {
            ceiling::unentitle(&pool, "reader", "main", &AuditActor::cli(), Utc::now())
                .await
                .expect("unentitle");
        }));
        ui.release_held();
        ui.poll_eval(
            "async () => (await (await caches.open('read-content')).keys()) \
             .every(k => !new URL(k.url).pathname.includes('/catalogs/main/'))",
        );
        // The account screen stays: only the eviction happened.
        assert!(ui.present("#account-user"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn show_the_whole_concept_after_losing_the_grant_evicts_the_catalog() {
    let server = TestServer::start_with(vec![("main", main_files())], |base| Config {
        public_url: Some(base.to_string()),
        concept_outline_chars: 20,
        ..Config::default()
    })
    .await;
    server.user_with_password("reader", EMAIL, PASSWORD).await;
    crate::support::server::applied(&server.app, async {
        ceiling::entitle(
            &server.db.pool,
            &NewEntitlement { user: "reader", catalog: "main", access: Access::Read },
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("entitle");
    })
    .await;
    let runtime = tokio::runtime::Handle::current();
    let (pool, app) = (server.db.pool.clone(), std::sync::Arc::clone(&server.app));
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        // Beta's body is over 20 characters: only its outline comes back.
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_visible("#show-full");
        runtime.block_on(crate::support::server::applied(&app, async {
            ceiling::unentitle(&pool, "reader", "main", &AuditActor::cli(), Utc::now())
                .await
                .expect("unentitle");
        }));
        ui.click("#show-full");
        ui.wait_visible("#catalog-gone");
        let counts = ui.eval(
            "async () => (await (await caches.open('read-content')).keys()).length",
            json!(null),
        );
        assert_eq!(counts, json!(0));
    })
    .await;
}
```

And three more for refusals that the screen that asked no longer shows, or that a second read of one screen brings:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_refusal_from_home_evicts_the_catalog_it_asked_about() {
    let (server, _) = server().await;
    let runtime = tokio::runtime::Handle::current();
    let (pool, app) = (server.db.pool.clone(), std::sync::Arc::clone(&server.app));
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        ui.open_reader("/v/other/notes.md");
        ui.wait_text("#concept-title", "Notes");
        // Home for main, whose recent changes wait in the page.
        ui.open_reader("/c");
        ui.hold_next("GET", "/catalogs/main/recent-changes?limit=20");
        ui.click("[data-catalog=main]");
        ui.wait_visible("#screen-home");
        // Switch to other, then lose main, then let main's answer through.
        ui.open_reader("/c");
        ui.click("[data-catalog=other]");
        ui.wait_visible("#recent");
        runtime.block_on(crate::support::server::applied(&app, async {
            ceiling::unentitle(&pool, "reader", "main", &AuditActor::cli(), Utc::now())
                .await
                .expect("unentitle");
        }));
        ui.release_held();
        ui.poll_eval(
            "async () => (await (await caches.open('read-content')).keys()) \
             .every(k => !new URL(k.url).pathname.includes('/catalogs/main/'))",
        );
        let kept = ui.eval(
            "async () => ({ other: (await (await caches.open('read-content')).keys()) \
             .some(k => new URL(k.url).pathname.includes('/catalogs/other/')), \
             chosen: localStorage.getItem('okf.read.catalog') })",
            json!(null),
        );
        assert_eq!(kept, json!({ "other": true, "chosen": "other" }));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_401_signs_out_without_waiting_for_a_sibling_read() {
    let (server, user_id) = server().await;
    let runtime = tokio::runtime::Handle::current();
    let pool = server.db.pool.clone();
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        runtime.block_on(async {
            user_sessions::revoke_all(&pool, &user_id, None).await.expect("revoke");
        });
        // Beta's links read waits in the page and is never released; its
        // concept read answers 401 at once.
        ui.hold_next("GET", "/links/systems/beta.md");
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_visible("#signin-email");
        let counts = ui.eval(CACHE_COUNTS, json!(null));
        assert!(counts.get("read-content").is_none(), "{counts}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_401_after_an_ordinary_refusal_still_signs_out() {
    let (server, user_id) = server().await;
    let runtime = tokio::runtime::Handle::current();
    let pool = server.db.pool.clone();
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        // A concept that is not there (404 at once) whose links read waits.
        ui.hold_next("GET", "/links/systems/missing.md");
        ui.open_reader("/v/main/systems/missing.md");
        ui.wait_visible("#screen-concept");
        runtime.block_on(async {
            user_sessions::revoke_all(&pool, &user_id, None).await.expect("revoke");
        });
        ui.release_held();
        ui.wait_visible("#signin-email");
        let counts = ui.eval(CACHE_COUNTS, json!(null));
        assert!(counts.get("read-content").is_none(), "{counts}");
    })
    .await;
}
```

Three more: a slow successful answer must not undo an eviction, a folder whose index is deleted between the two reads still lists its contents, and a long concept's outline links reach their heading.

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_success_cannot_put_back_an_evicted_catalog() {
    let (server, _) = server().await;
    let runtime = tokio::runtime::Handle::current();
    let (pool, app) = (server.db.pool.clone(), std::sync::Arc::clone(&server.app));
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        // Beta's 200 is held in the worker after the server answered it,
        // and the page learns when the worker has finished with it.
        let mut paused = ui.pause_worker_responses("*/concepts/systems/beta.md");
        ui.hold_answer_later("GET", "/concepts/systems/beta.md");
        ui.open_reader("/v/main/systems/beta.md");
        let request = paused.next_paused();
        // The grant goes, and a tree read makes the page evict main.
        runtime.block_on(crate::support::server::applied(&app, async {
            ceiling::unentitle(&pool, "reader", "main", &AuditActor::cli(), Utc::now())
                .await
                .expect("unentitle");
        }));
        ui.open_reader("/t/main");
        ui.wait_visible("#catalog-gone");
        // Now the old 200 reaches the worker.
        paused.release(&request);
        ui.poll_eval("() => window.heldFetched === true");
        let main_entries = ui.eval(
            "async () => (await (await caches.open('read-content')).keys()) \
             .filter(k => new URL(k.url).pathname.includes('/catalogs/main/')).length",
            json!(null),
        );
        assert_eq!(main_entries, json!(0));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_folder_whose_index_is_deleted_meanwhile_still_lists_its_concepts() {
    let (server, _) = server().await;
    let runtime = tokio::runtime::Handle::current();
    let (pool, app) = (server.db.pool.clone(), std::sync::Arc::clone(&server.app));
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/t/main/systems");
        ui.wait_text("#body", "Systems");
        // Again: the tree is read, the index read waits, the index goes.
        ui.hold_next("GET", "/documents/systems/index.md");
        ui.open_reader("/t/main");
        ui.wait_visible("#folders");
        ui.open_reader("/t/main/systems");
        ui.wait_visible("#screen-tree");
        let mut without_index = main_files();
        without_index.remove("systems/index.md");
        runtime.block_on(publish(&pool, &app, "main", without_index));
        ui.release_held();
        ui.wait_text("#concepts", "Alpha");
        assert!(!ui.present("#body"), "no index body");
        assert!(!ui.present("#concept-gone"), "not treated as a missing concept");
        let cached = ui.eval(
            "async () => (await (await caches.open('read-content')).keys()) \
             .some(k => new URL(k.url).pathname.endsWith('/documents/systems/index.md'))",
            json!(null),
        );
        assert_eq!(cached, json!(false));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_outline_link_in_a_long_concept_keeps_the_whole_body() {
    let server = TestServer::start_with(vec![("main", main_files())], |base| Config {
        public_url: Some(base.to_string()),
        concept_outline_chars: 20,
        ..Config::default()
    })
    .await;
    server.user_with_password("reader", EMAIL, PASSWORD).await;
    crate::support::server::applied(&server.app, async {
        ceiling::entitle(
            &server.db.pool,
            &NewEntitlement { user: "reader", catalog: "main", access: Access::Read },
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("entitle");
    })
    .await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/v/main/systems/beta.md", EMAIL, PASSWORD);
        ui.click("#show-full");
        ui.wait_text("#body", "Two settings");
        ui.click("#outline summary");
        ui.click("#outline a");
        ui.wait_text("#body", "Two settings");
        ui.poll_eval(
            "() => { const heading = document.querySelector('[data-slug=\"settings\"]'); \
             return !!heading && heading.getBoundingClientRect().top < innerHeight; }",
        );
    })
    .await;
}
```

Two more, for a slow success that answers after its document was deleted and evicted, one per kind of document:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_success_cannot_put_back_a_deleted_concept() {
    let (server, _) = server().await;
    let runtime = tokio::runtime::Handle::current();
    let (pool, app) = (server.db.pool.clone(), std::sync::Arc::clone(&server.app));
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        // Alpha's 200 waits in the worker; the page learns when it arrives.
        let mut paused = ui.pause_worker_responses("*/concepts/systems/alpha.md*");
        ui.hold_answer_later("GET", "/concepts/systems/alpha.md");
        ui.open_reader("/v/main/systems/alpha.md");
        let old = paused.next_paused();
        let mut without_alpha = main_files();
        without_alpha.remove("systems/alpha.md");
        runtime.block_on(publish(&pool, &app, "main", without_alpha));
        // A second read of alpha gets the 404, and the page evicts alpha.
        ui.open_reader("/me");
        ui.open_reader("/v/main/systems/alpha.md");
        let fresh = paused.next_paused();
        paused.release(&fresh);
        ui.wait_visible("#concept-gone");
        // Now the old 200 reaches the worker.
        paused.release(&old);
        ui.poll_eval("() => window.heldFetched === true");
        let cached = ui.eval(
            "async () => (await (await caches.open('read-content')).keys()) \
             .some(k => new URL(k.url).pathname.endsWith('/concepts/systems/alpha.md'))",
            json!(null),
        );
        assert_eq!(cached, json!(false));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_success_cannot_put_back_a_deleted_folder_index() {
    let (server, _) = server().await;
    let runtime = tokio::runtime::Handle::current();
    let (pool, app) = (server.db.pool.clone(), std::sync::Arc::clone(&server.app));
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        let mut paused = ui.pause_worker_responses("*/documents/systems/index.md*");
        ui.hold_answer_later("GET", "/documents/systems/index.md");
        ui.open_reader("/t/main/systems");
        let old = paused.next_paused();
        let mut without_index = main_files();
        without_index.remove("systems/index.md");
        runtime.block_on(publish(&pool, &app, "main", without_index));
        // A fresh tree no longer lists the index, so the folder screen would
        // not read it again: make the eviction tree.js makes on its 404
        // (journey `a_folder_whose_index_is_deleted_meanwhile...`) directly.
        ui.eval(
            "async () => { await (await import('/read/js/cache.js')) \
             .evictDocument('main', 'documents', '/systems/index.md'); return true; }",
            json!(null),
        );
        paused.release(&old);
        ui.poll_eval("() => window.heldFetched === true");
        let cached = ui.eval(
            "async () => (await (await caches.open('read-content')).keys()) \
             .some(k => new URL(k.url).pathname.endsWith('/documents/systems/index.md'))",
            json!(null),
        );
        assert_eq!(cached, json!(false));
    })
    .await;
}
```

The folder journey needs this helper in `tests/ui_e2e/read.rs` (as `tests/ui_e2e/recent.rs` imports):

```rust
/// Imports `entries` over everything `catalog` holds and applies the change
/// messages, as a publish would.
async fn publish(
    pool: &sqlx::PgPool,
    app: &std::sync::Arc<okf_mcp::app::App>,
    catalog: &str,
    entries: okf_mcp::archive::Files,
) {
    let options = okf_mcp::store::import::ImportOptions {
        replace: true,
        scope: okf_mcp::store::import::ImportScope::Everything,
        expect_generation: None,
        expect_catalog_id: None,
        actor: "editor",
        audit: &AuditActor::cli(),
        now: Utc::now(),
    };
    crate::support::server::applied(app, async {
        okf_mcp::store::import::import(pool, catalog, entries, &options)
            .await
            .expect("import");
    })
    .await;
}
```

(`Files` is a map from path to bytes; if its `remove` takes a different key type, rebuild the file list without `systems/index.md` instead.)

The slow-success journey needs a DevTools helper in `tests/ui_e2e/harness.rs` that pauses the worker's responses after the server has answered them (the `Fetch` domain at the `Response` stage, on the `service_worker` target):

```rust
/// Responses paused in the reader's service worker (see
/// [`Ui::pause_worker_responses`]); dropping it ends the pausing.
pub struct PausedResponses {
    devtools: DevTools,
    session: String,
}

impl PausedResponses {
    /// Waits for the next paused response and returns its request id.
    pub fn next_paused(&mut self) -> String {
        let deadline = Instant::now() + WAIT;
        let params = self.devtools.event("Fetch.requestPaused", deadline, |_| true);
        params["requestId"].as_str().expect("a request id").to_string()
    }

    /// Lets the paused response `request` through to the worker.
    pub fn release(&mut self, request: &str) {
        self.devtools.call(
            Some(&self.session),
            "Fetch.continueResponse",
            serde_json::json!({ "requestId": request }),
        );
    }
}
```

and in `impl Ui`:

```rust
    /// Pauses every response the reader's service worker gets for a URL
    /// matching `pattern` (DevTools wildcards), after the server answered.
    pub fn pause_worker_responses(&self, pattern: &str) -> PausedResponses {
        let mut devtools = DevTools::connect(&self.browser);
        let targets = devtools.call(None, "Target.getTargets", serde_json::json!({}));
        let worker = targets["targetInfos"]
            .as_array()
            .expect("targets")
            .iter()
            .find(|target| target["type"] == "service_worker")
            .and_then(|target| target["targetId"].as_str())
            .expect("the reader's service worker")
            .to_string();
        let attached = devtools.call(
            None,
            "Target.attachToTarget",
            serde_json::json!({ "targetId": worker, "flatten": true }),
        );
        let session = attached["sessionId"].as_str().expect("a session").to_string();
        devtools.call(
            Some(&session),
            "Fetch.enable",
            serde_json::json!({ "patterns": [{ "urlPattern": pattern, "requestStage": "Response" }] }),
        );
        PausedResponses { devtools, session }
    }
```

and, also in `impl Ui`, a variant of `hold_answer` that does not wait for the answer (`hold_answer` polls for it, which would never end while the worker holds it):

```rust
    /// Sends the page's next `method` request to a path ending in `suffix`
    /// at once and holds only its answer in the page; `window.heldFetched`
    /// turns true when the answer arrives (see [`HOLD_NEXT`]). Unlike
    /// [`Ui::hold_answer`], it returns at once.
    pub fn hold_answer_later(&self, method: &str, suffix: &str) {
        self.eval(
            HOLD_NEXT,
            serde_json::json!({ "method": method, "suffix": suffix, "early": true }),
        );
    }
```

`window.heldFetched` turning true is the journey's signal that the worker has finished with the old 200: the worker answers the page only after its cache write (or refusal to write).

The two-account and other-build journeys need two more harness pieces. In `impl Ui`:

```rust
    /// Runs `visit` on a second tab of the same browser (same cookies), and
    /// closes it after.
    pub fn in_second_tab(&self, visit: impl FnOnce(&Page)) {
        let tab = self.browser.new_page().expect("open a second tab");
        visit(&tab);
        drop(tab);
    }

    /// While the returned guard lives, every service worker that starts gets
    /// `body` instead of the server's answer for URLs matching `pattern`
    /// (DevTools: auto-attach to new service workers paused, enable `Fetch`
    /// on them, then let them run). A background thread answers the
    /// DevTools events.
    pub fn tamper_new_workers(&self, pattern: &str, body: &str) -> Tamper {
        Tamper::start(DevTools::connect(&self.browser), pattern, body)
    }
```

and beside `DevTools`:

```rust
/// Rewrites matching responses for every new service worker until dropped
/// (see [`Ui::tamper_new_workers`]).
pub struct Tamper {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Tamper {
    fn start(mut devtools: DevTools, pattern: &str, body: &str) -> Self {
        devtools.call(
            None,
            "Target.setAutoAttach",
            serde_json::json!({
                "autoAttach": true,
                "waitForDebuggerOnStart": true,
                "flatten": true,
                "filter": [{ "type": "service_worker" }],
            }),
        );
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (pattern, body) = (pattern.to_string(), base64_encode(body.as_bytes()));
        let stopping = std::sync::Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !stopping.load(std::sync::atomic::Ordering::Relaxed) {
                let Some(event) = devtools.read_json() else { continue };
                let session = event["sessionId"].as_str().map(str::to_string);
                match event["method"].as_str() {
                    Some("Target.attachedToTarget") => {
                        let worker = event["params"]["sessionId"].as_str().expect("a session").to_string();
                        devtools.send(Some(&worker), "Fetch.enable", serde_json::json!({
                            "patterns": [{ "urlPattern": pattern, "requestStage": "Response" }],
                        }));
                        devtools.send(Some(&worker), "Runtime.runIfWaitingForDebugger", serde_json::json!({}));
                    }
                    Some("Fetch.requestPaused") => {
                        devtools.send(session.as_deref(), "Fetch.fulfillRequest", serde_json::json!({
                            "requestId": event["params"]["requestId"],
                            "responseCode": 200,
                            "responseHeaders": [{ "name": "Content-Type", "value": "text/css; charset=utf-8" }],
                            "body": body,
                        }));
                    }
                    _ => {}
                }
            }
        });
        Self { stop, thread: Some(thread) }
    }
}

impl Drop for Tamper {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Standard base64 (DevTools wants response bodies in it).
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (at, byte)| n | u32::from(*byte) << (16 - 8 * at));
        for at in 0..4 {
            if at <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * at) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}
```

`DevTools::send` is `call` without waiting for the answer (the thread reads answers and events in one loop): add it beside `call`, sending `{"id", "method", "params", "sessionId"}` as `try_call` does and returning at once. If Chromium does not auto-attach service workers from the browser session, attach on `Target.targetCreated` instead (`Target.setDiscoverTargets`), accepting that the first request may slip through, and say so in the journey.

Add the polling helper these use to `impl Ui` in `tests/ui_e2e/harness.rs`:

```rust
    /// Waits until the JS function `function` (no argument) returns `true`.
    pub fn poll_eval(&self, function: &str) {
        self.poll(function, || self.eval(function, serde_json::json!(null)) == serde_json::json!(true));
    }
```

`fail_next_kind` wraps `window.fetch` in the page, and the page navigates by hash only, so the wrapper survives the navigation to beta.

- [ ] **Step 2: Run them**

Run: `scripts/test-pg.sh run cargo test --test ui_e2e read:: -- --test-threads=2`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/ui_e2e/read.rs tests/ui_e2e/harness.rs
git commit -m "test(read): revoked sessions, removed grants, late refusals and unlock refusals

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: Documentation and changelog

**Files:**
- Create: `docs/reference/reader.md`
- Modify: `README.md` (one line in its feature or documentation list)
- Create: `changelog/<UTC timestamp>-mobile-reader.md` (timestamp from `date -u +%Y-%m-%dT%H-%M-%SZ`)

- [ ] **Step 1: Write `docs/reference/reader.md`**

```markdown
# Mobile reader

A read-only web app at `/read/` for reading a catalog on a phone. It signs
in with the same account as the web console and needs no API key.

## Install

- **iPhone and iPad:** open `https://<your server>/read/` in Safari, tap
  Share, then Add to Home Screen. Sign in once inside the installed app (it
  keeps its own cookies); a passkey makes that quick.
- **Android:** open the same address in Chrome and accept the install
  prompt, or choose Install app from the menu.

## What it shows

Home (search and recent changes), the catalog picker, the folder tree (each
folder's `index.md` first), concepts with their outline and what links to
them, search results, and the account screen with sign-out.

## Offline

- The concepts and folder pages you open are kept on the device, up to 200
  concepts; the least recently viewed is dropped first. The tree, recent
  changes and backlinks you have seen are kept too.
- Offline, the reader shows those copies with "Offline. Synced ..." and
  Home lists what is available. Search needs a connection.
- The copies are deleted when you sign out, when the server says the
  session has ended (expired, revoked, or the account removed), and, for one
  catalog, when you can no longer read it. A catalog that needs an unlock or
  whose team is suspended keeps its copies; unlock it in the web console.
- iOS may clear an installed app's storage after weeks without use.

## For maintainers

- Files: `src/http/read/` (embedded by `build.rs` as `READ_ASSETS`), served
  by `src/http/read.rs`. The worker is `src/http/read/sw.js`; its version is
  a hash of every file it precaches (the reader's and the console's Pico,
  `dom.js` and `format.js`), so any change to them reaches installed apps
  on their next launch.
- Styling is `src/http/read/read.css` on Pico CSS, independent of the
  console's `styles.css`. Both style the HTML `src/render.rs` writes into
  `body_html`.
- Before a release, check by hand: install on an iPhone and on Android,
  standalone launch, safe areas on a notched screen, light and dark, and
  reading in airplane mode.
```

- [ ] **Step 2: Link it from `README.md`**

Find the list of reference documents in `README.md` (search for `docs/reference/`) and add, in the same style:

```markdown
- [Mobile reader](docs/reference/reader.md): read a catalog on a phone at `/read/`, offline included.
```

- [ ] **Step 3: Write the changelog entry**

```markdown
## Added
- A mobile reader at `/read/`: an installable, read-only web app (iPhone through Add to Home Screen, Android through Chrome's install prompt) with the catalog picker, folder tree, concepts with outline and backlinks, search, recent changes and account. It signs in with the console's session cookie.
- Offline reading: a service worker scoped to `/read/` precaches the reader and keeps network-first copies of opened concepts and folder pages (up to 200, least recently viewed out first) and of trees, recent changes and backlinks.
- `GET /read/shell.json` lists the files the worker precaches, each with the SHA-256 of the bytes this build serves; the worker installs only when the listing's version is its own and every file hashes as listed. `/read/sw.js` carries, as its version, a hash of every precached file (the shared console files included).
- Every `/api/v1` answer carries `X-OKF-Account`, an opaque 16-hex-digit tag of the account it was made for; the reader's worker uses it to keep copies to one account.

## Changed
- `build.rs` embeds `src/http/read` as `READ_ASSETS` beside the console's `ASSETS`, through one shared function.
- Sign-in's `return_to` accepts `/read/` paths.
- The console's asset handler serves `png` and `webmanifest` types.

## Notes
- Cached copies are wiped on sign-out and on any `401`, including one that answers after the screen that asked was left or a read a button made; a lost catalog grant (`403 Forbidden`, `404 CatalogNotSelected`) evicts that catalog only; `UnlockRequired` and `Suspended` keep them.
- Content reads are network-first rather than stale-while-revalidate, so a revoked session never sees cached content while online.
```

- [ ] **Step 4: Commit**

```bash
git add docs/reference/reader.md README.md changelog
git commit -m "docs(read): reader reference, README link and changelog

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13: Full gates

- [ ] **Step 1: Run every gate**

Run each and expect exit 0:

```bash
tools/agentic/gate.sh fmt
tools/agentic/gate.sh lint
tools/agentic/gate.sh test
tools/agentic/gate.sh test-ui
tools/agentic/gate.sh dylint
```

- [ ] **Step 2: Fix anything a gate reports, commit, and re-run that gate.**

- [ ] **Step 3: Manual checks (needs a phone and a deployed build)**

On an iPhone (Safari, Share, Add to Home Screen) and on Android (Chrome install): the app opens standalone; content clears the notch and home indicator; light and dark follow the system; open three concepts, switch on airplane mode, relaunch, and read them with the offline banner.

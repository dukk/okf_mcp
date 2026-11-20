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
| `src/http/read/index.html` | The reader's page: head metadata, bar, banner, main, bottom tabs. |
| `src/http/read/manifest.webmanifest` | Install metadata. |
| `src/http/read/icons/*.png` | App icons (192, 512, maskable 512, Apple 180). |
| `src/http/read/read.css` | The reader's own tokens, light and dark, phone layout, body markup. |
| `src/http/read/sw.js` | The service worker. |
| `src/http/read/js/names.js` | Cache names and the cap, shared by page and worker (classic script). |
| `src/http/read/js/api.js` | Read-only client: `read`, `readAll`, `signOut`, `classify`, `KIND`, `ReadError`, `enc`, `encPath`. |
| `src/http/read/js/cache.js` | Page-side cache upkeep: `wipe`, `claimFor`, `evictCatalog`, `evictConcept`, `offlineDocuments`. |
| `src/http/read/js/route.js` | Hash routes: `parse`, `hrefs`, `hrefFor`, `fromBrowse`, `parentOf`. |
| `src/http/read/js/main.js` | Boot, session, worker registration, screen dispatch, freshness banner, failure handling, tabs. |
| `src/http/read/js/parts.js` | Shared view parts: `searchForm`, `crumbs`, `renderBody`, `message`. |
| `src/http/read/js/listing.js` | `folderListing`: one folder's subfolders, index and concepts from the tree. |
| `src/http/read/js/screens/{home,catalogs,tree,search,concept,me}.js` | One screen each. |
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
- Modify: `src/http/mod.rs:18` (module list) and `:413` (`open.merge(ui::routes())`)
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
        let files: Vec<&str> = listing["files"]
            .as_array()
            .expect("files")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        assert!(files.contains(&"/read/"));
        assert!(!files.contains(&"/read/index.html"));
        assert!(files.contains(&"/read/manifest.webmanifest"));
        for shared in SHARED_CONSOLE_FILES {
            assert!(files.contains(shared), "{shared}");
        }
        // Each listed file is served, so `cache.addAll` cannot fail on one.
        for file in files {
            assert_eq!(get(file).await.status(), StatusCode::OK, "{file}");
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
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;

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

/// `GET /read/shell.json`: `{"version": ..., "files": [...]}`, what the
/// worker precaches. `index.html` is listed as `/read/`, the URL the page
/// is opened at.
#[tracing::instrument(level = "debug", skip_all)]
async fn shell() -> Json<serde_json::Value> {
    let files: Vec<String> = READ_ASSETS
        .iter()
        .map(|(name, _)| match *name {
            "index.html" => "/read/".to_string(),
            other => format!("/read/{other}"),
        })
        .chain(SHARED_CONSOLE_FILES.iter().map(|file| (*file).to_string()))
        .collect();
    Json(serde_json::json!({ "version": SHELL_VERSION.as_str(), "files": files }))
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

In `src/http/mod.rs`, add `mod read;` beside `mod ui;` (line 18), and change `open.merge(ui::routes())` (line 413) to `open.merge(ui::routes()).merge(read::routes())`.

- [ ] **Step 10: Run the tests to see them pass**

Run: `scripts/test-pg.sh run cargo test --lib http::read http::ui`
Expected: PASS, every test in both modules.

- [ ] **Step 11: Gates and commit**

Run: `tools/agentic/gate.sh fmt` then `tools/agentic/gate.sh lint`. Expected: both exit 0.

```bash
git add build.rs src/http/ui.rs src/http/read.rs src/http/mod.rs src/http/read
git commit -m "feat(read): embed and serve the mobile reader's files under /read/

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Sign-in can return to the reader

**Files:**
- Modify: `src/http/pages.rs:195-205` (`is_safe_return_to`) and its tests (`:317-339`)

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

```js
// The reader's service worker (reader design, "Caching"). Its scope is
// /read/, so it never controls a console page. The shell is precached and
// served cache-first; concept and document reads (read-content, capped) and
// listings (read-meta) are network-first with the cache as the offline
// fallback. Only 2xx answers are cached. The worker never makes an error
// answer of its own; the page decides what every refusal means.
importScripts('/read/js/names.js');

const SHELL = `${READ_CACHES.shellPrefix}__SHELL_VERSION__`;
const CONTENT = /^\/api\/v1\/catalogs\/[^/]+\/(concepts|documents)\//;
const META = [
    /^\/api\/v1\/catalogs$/,
    /^\/api\/v1\/catalogs\/[^/]+\/(tree|recent-changes)$/,
    /^\/api\/v1\/catalogs\/[^/]+\/links\//,
];

self.addEventListener('install', event => {
    event.waitUntil((async () => {
        const listing = await (await fetch('/read/shell.json', { cache: 'no-store' })).json();
        const cache = await caches.open(SHELL);
        await cache.addAll(listing.files);
        await self.skipWaiting();
    })());
});

self.addEventListener('activate', event => {
    event.waitUntil((async () => {
        const names = await caches.keys();
        await Promise.all(names
            .filter(name => name.startsWith(READ_CACHES.shellPrefix) && name !== SHELL)
            .map(name => caches.delete(name)));
        await self.clients.claim();
    })());
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
    // Any other API read (session, search, whoami) goes to the network as is.
});

// The shell's copy of `key`, else the network's answer to `request`.
async function fromShell(key, request) {
    return (await caches.match(key, { cacheName: SHELL })) ?? fetch(request);
}

// The network's answer, remembered when it is a 2xx; without a network,
// the remembered copy marked X-Read-Offline. `capped` caches keep only the
// newest READ_CACHES.contentCap entries.
async function networkFirst(request, cacheName, capped) {
    const cache = await caches.open(cacheName);
    let response;
    try {
        response = await fetch(request);
    } catch (failure) {
        const cached = await cache.match(request);
        if (!cached) throw failure;
        if (capped) await renew(cache, request, cached.clone());
        return stamped(cached, 'X-Read-Offline', '1');
    }
    if (response.ok) {
        await renew(cache, request, stamped(response.clone(), 'X-Read-Cached-At', new Date().toISOString()));
        if (capped) await trim(cache);
    }
    return response;
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

const { content, meta, owner } = self.READ_CACHES;
const OWNER_KEY = '/read/__owner';

// Deletes every cached catalog read and the record of whose they were.
export async function wipe() {
    await Promise.all([content, meta, owner].map(name => caches.delete(name)));
}

// Makes the caches `user`'s: wipes them when they were someone else's.
export async function claimFor(user) {
    const store = await caches.open(owner);
    const recorded = await store.match(OWNER_KEY);
    if (recorded && (await recorded.text()) === user) return;
    await wipe();
    await (await caches.open(owner)).put(OWNER_KEY, new Response(user));
}

// Deletes the entries of `cacheNames` whose URL path `matches`.
async function evict(cacheNames, matches) {
    await Promise.all(cacheNames.map(async name => {
        const cache = await caches.open(name);
        const keys = await cache.keys();
        await Promise.all(keys.filter(key => matches(new URL(key.url).pathname)).map(key => cache.delete(key)));
    }));
}

// Deletes everything cached for `catalog`.
export function evictCatalog(catalog) {
    const prefix = `/api/v1/catalogs/${enc(catalog)}/`;
    return evict([content, meta], path => path.startsWith(prefix));
}

// Deletes the cached copies (short and full) of one concept.
export function evictConcept(catalog, path) {
    const exact = `/api/v1/catalogs/${enc(catalog)}/concepts/${encPath(path)}`;
    return evict([content], url => url === exact);
}

// The concepts and documents of `catalog` readable offline, newest first,
// one per path: [{ path, title, cachedAt }].
export async function offlineDocuments(catalog) {
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
    me: () => '#/me',
});

// The folder holding `path` ('a/b/c.md' -> 'a/b').
export function parentOf(path) {
    return segments(path).slice(0, -1).join('/');
}

// Where a document opens: a folder's index.md opens its folder, anything
// else the concept view.
export function hrefFor(catalog, path, section = null) {
    const parts = segments(path);
    if (parts[parts.length - 1] === 'index.md') return hrefs.tree(catalog, parts.slice(0, -1).join('/'));
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
import { claimFor, wipe, evictCatalog, evictConcept } from './cache.js';
import { parse, hrefs, parentOf } from './route.js';
import { message } from './parts.js';
import * as home from './screens/home.js';
import * as catalogs from './screens/catalogs.js';
import * as tree from './screens/tree.js';
import * as search from './screens/search.js';
import * as concept from './screens/concept.js';
import * as me from './screens/me.js';

const SCREENS = { home, catalogs, tree, search, concept, me };
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
    const active = { home: 'tab-home', tree: 'tab-browse', concept: 'tab-browse', search: 'tab-search', me: 'tab-me' }[current];
    for (const tab of document.querySelectorAll('.tabs a')) {
        if (tab.id === active) tab.setAttribute('aria-current', 'page');
        else tab.removeAttribute('aria-current');
    }
}

// Acts on a refusal. The cache and session side effects always happen, even
// when the screen that asked has been left (`live()` false): a 401 or a lost
// grant is true whichever screen shows now. Only the drawing is skipped then.
async function fail(view, route, error, live) {
    switch (error.kind) {
        case KIND.SIGNED_OUT:
            await signIn();
            return;
        case KIND.CATALOG_GONE: {
            const catalog = route.catalog ?? state.catalog;
            if (catalog) await evictCatalog(catalog);
            if (catalog === state.catalog) choose(null);
            if (live()) location.hash = hrefs.catalogs(catalog);
            return;
        }
        case KIND.CONCEPT_GONE:
            await evictConcept(route.catalog, route.path);
            if (!live()) return;
            clear(view).append(message('This concept no longer exists.',
                h('a', { href: hrefs.tree(route.catalog, parentOf(route.path)), id: 'gone-folder' }, 'Open its folder'), 'concept-gone'));
            return;
        case KIND.LOCKED:
            if (!live()) return;
            clear(view).append(message(error.message,
                error.body?.error === 'UnlockRequired' ? h('a', { href: '/ui/', id: 'unlock-link' }, 'Unlock in the console') : null, 'locked'));
            return;
        case KIND.UNAVAILABLE:
            if (!live()) return;
            clear(view).append(message('Not available offline.',
                h('a', { href: hrefs.home(), id: 'offline-list-link' }, 'See what is available offline'), 'unavailable'));
            return;
        case KIND.FAILED:
            if (!live()) return;
            clear(view).append(message(error.message, null, 'failed'));
            return;
    }
}

async function show() {
    const route = parse(location.hash);
    const mine = ++generation;
    if ((route.catalog ?? null) !== null && route.catalog !== state.catalog) choose(route.catalog);
    tabs();
    $('#offline-banner').hidden = true;
    if (NEEDS_CATALOG.has(route.screen) && !state.catalog) {
        location.replace(hrefs.catalogs());
        return;
    }
    const view = h('section', { class: 'screen', id: `screen-${route.screen}` });
    clear($('#reader')).append(view);
    const live = () => mine === generation;
    // `fail` is for reads a screen makes later (a button's), so they get the
    // same handling as the first render's.
    const ctx = { state, route, choose, freshness, live, fail: error => fail(view, route, error, live) };
    try {
        await SCREENS[route.screen].render(view, ctx);
    } catch (error) {
        if (!(error instanceof ReadError)) throw error;
        await ctx.fail(error);
    }
}

function registerWorker() {
    if (!('serviceWorker' in navigator)) return;
    navigator.serviceWorker.register('/read/sw.js', { scope: '/read/' }).catch(error => {
        // The reader still works online; only offline reading is lost.
        console.warn('offline reading is unavailable:', error.message);
    });
}

async function boot() {
    registerWorker();
    try {
        const { data } = await read('/session');
        state.session = data;
        await claimFor(data.user);
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
// Account: who is signed in, until when, and sign-out (which deletes every
// offline copy first, even if the server cannot be reached).
import { h } from '/ui/js/core/dom.js';
import { when } from '/ui/js/core/format.js';
import { signOut, ReadError } from '../api.js';
import { wipe } from '../cache.js';
import { message } from '../parts.js';

export async function render(view, ctx) {
    const session = ctx.state.session;
    view.append(h('h1', {}, 'Account'));
    if (!session) {
        view.append(message('Account details need a connection.', null, 'account-offline'));
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
        button);
}
```

- [ ] **Step 6: Write minimal screens so `main.js` loads**

Each of `home.js`, `catalogs.js`, `tree.js`, `search.js`, `concept.js` in `src/http/read/js/screens/` starts as the following, with its screen name in the text; Tasks 7 to 9 replace each one completely:

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
            // Not cached and offline: the lists below still work.
            if (error.kind !== KIND.UNAVAILABLE) throw error;
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
    const [result, links] = await Promise.all([read(conceptUrl), backlinks(base, path)]);
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

- [ ] **Step 4: Run it to see it pass**

Run: `scripts/test-pg.sh run cargo test --test ui_e2e read:: -- --test-threads=2`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/http/read/js/screens/concept.js tests/ui_e2e/read.rs
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
    const catalog = ctx.state.catalog;
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

Add the polling helper both use to `impl Ui` in `tests/ui_e2e/harness.rs`:

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
- `GET /read/shell.json` lists the files the worker precaches; `/read/sw.js` carries, as its version, a hash of every one of them (the shared console files included).

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

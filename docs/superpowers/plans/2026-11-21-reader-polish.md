# Mobile Reader Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Read `AGENTS.md`, `tools/agentic/README.md`, `tools/agentic/brief-common.md` and `tools/agentic/lessons.md` first: every gate runs through `tools/agentic/gate.sh`.

**Goal:** Give the mobile reader at `/read/` a clean, Notion-like design (titles instead of paths, an activity timeline, serif reading, icons, self-hosted type) and fix the six functional bugs found on a phone.

**Architecture:** The server adds three read-only fields (titles and descriptions on recent changes, descriptions on the tree, plain-text search snippets) and lets pages load their own fonts. The reader gets a design layer in `read.css` (tokens and component classes on top of Pico), an SVG icon sprite and two self-hosted OFL fonts, all precached by the existing shell listing. Presentational grouping (timeline batches, outline, backlink counts, avatars) lives in small pure ES modules with Node tests; each screen is then rebuilt from shared view parts in `parts.js`.

**Tech Stack:** Rust (axum, pulldown-cmark), vanilla ES modules, Pico CSS 2, SVG sprite with `<use>`, woff2 (fonttools through `uvx`), Node's built-in test runner, rustwright headless Chromium journeys.

**Spec:** `docs/superpowers/specs/2026-11-21-reader-polish-design.md`. Visual reference: `docs/superpowers/specs/assets/2026-11-21-reader-polish-mockups.html` (its CSS is the source of the tokens and component classes below).

**Previous milestone:** `docs/superpowers/plans/2026-11-20-mobile-reader-pwa.md`. Its early snippets are out of date: the shipped code under `src/http/read/` is the truth, and every snippet below was written against it.

---

## Rules for every task

- **Front end** (`.claude/rules/wasm-frontend.md`): Pico CSS and vanilla JS only. No inline `style` attributes or inline scripts: the CSP forbids them, so every colour variant is a class. Data reaches the DOM through `h()`, `fill()` and `textContent`; the only `innerHTML` is `body_html` (in `parts.js` `renderBody`). Never pass `null` to the native `append`/`replaceChildren`: use `h()` or `fill()`, which drop it (issue #302).
- **SVG:** `h()` makes HTML elements. Icons are made only through `icon()` in `src/http/read/js/icons.js` (Task 4), which uses the SVG namespace.
- **Rust** (`.claude/CLAUDE.md`): doc comments with Arguments/Returns/Errors on every item, no `.unwrap()` outside tests, comments on Rust idioms for a Python reader, `#[tracing::instrument(...)]` on every function or the `hyp_tracing` allow attribute with a reason (as the surrounding code does).
- **No real infrastructure details** in any file, test or commit: use `example.com`, `example.internal` and neutral names (`alex`, `sam`, `reader`).
- **No emoji** anywhere. Typographic characters in JS strings are written as escapes (`'·'` for the middle dot, `'›'` for the breadcrumb chevron).
- **Element ids** the journeys use keep their meaning: `#recent`, `#folders`, `#concepts`, `#backlinks`, `#outline`, `#body`, `#search-form`, `#search-input`, `#hits`, `#offline-list`, `#concept-title`, `#document-title`, `#catalog-list`, `#account-user`, `#sign-out`, `#delete-offline`, `#tab-home`, `#tab-browse`, `#tab-search`, `#tab-me`, `#bar-catalog`, `#offline-banner`, `#reader`, `#screen-<name>`, and every notice id (`#concept-gone`, `#locked`, `#unavailable`, `#failed`, `#catalog-gone`, `#no-catalogs`, `#search-offline`, `#offline-list-link`, `#gone-folder`, `#unlock-link`, `#install-hint`, `#account-offline`, `#sign-out-failed`, `#offline-deleted`). Where a journey depends on markup a task changes, that task updates the journey.
- **Gates** (all through `tools/agentic/gate.sh`): `fmt`, `lint`, `test`, `test-ui`, `dylint`, `worker` (the reader's Node tests, added by Task 0), and `narrow` (cargo args passed straight through). Narrow runs that need Postgres get a reusable server first:

  ```bash
  export TEST_PG_URL=$(scripts/test-pg.sh up okf-mcp-test-pg-polish)
  tools/agentic/gate.sh narrow --lib -- http::api::browse
  tools/agentic/gate.sh narrow --test ui_e2e -- read::the_header --test-threads=2
  ```

  Remove it at the end of the task with `scripts/test-pg.sh down okf-mcp-test-pg-polish`.
- **Scratch files** (downloads, screenshots, helper scripts that are not committed) go in your session's scratchpad directory, written `$SCRATCH` below; never in the repository.
- **Changelog:** every task ends by writing `changelog/<UTC timestamp>-reader-polish-<slug>.md` (timestamp from `date -u +%Y-%m-%dT%H-%M-%SZ`), in the style of the existing entries (`## Added`, `## Changed`, `## Fixed`, `## Tests`, `## Docs`).
- **Commits** end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **The plan is authoritative.** Where the code has to differ, correct this plan in the same PR with a dated note ("Corrected (issue #N): ...") at the step that changed.

---

## Changes from the spec

These follow from the shipped code or from project rules; the spec's intent is kept.

1. **Search "Show more" asks again with a larger `limit`.** `GET /search` has no paging cursor (`src/mcp/search.rs`: one ranked list, capped at `MAX_LIMIT` 200). Its order is total (rank, catalog, path), so the first N hits of a larger request are the N already shown: "Show more" re-reads with `limit` + 50 and appends only the new hits. At 200 the count reads "200+ results" and asks to refine the search. Adding a cursor to the search tool is out of scope.
2. **"Edited 1 h ago by Name"** comes from the existing history route (`GET /catalogs/{c}/history/{path}?limit=1`), not a new field. That route is not cached by the worker, so offline the meta row leaves the edit out.
3. **Fonts are built from local release files, not a pinned URL.** The no-hostnames rule keeps download URLs out of the repository: `scripts/subset-reader-fonts.sh` takes the unpacked upstream release folders as arguments, and `scripts/reader-fonts.sha256` records the outputs so a rebuild can be checked.
4. **A theme choice on Account** (System, Light, Dark). The project's front-end rule asks for adaptive themes with a toggle; the spec is silent. The choice sets `data-theme` on `<html>` (the mockup's tokens already support it) and the `theme-color` metas.
5. **The folder screen no longer reads `index.md`.** Its lede is the index's `description` from the tree, and the About row opens the index as a document (`#/d/<catalog>/<folder>/index.md`). So the two journeys that deleted a folder's index under the folder screen now do it under the About screen, and `tree.test.mjs`'s index-read tests are replaced by tests of what the folder draws from the tree.
6. **Journeys are written in the task that changes the behaviour** (test first), not in one late task: Task 4 (scroll reset, header label), Task 5 (grouped import, Show older, Jump back in), Task 7 (level-1 outline, deduplicated backlinks, long concept whole), Task 8 (Show more past 50, no markdown in snippets), Task 9 (theme). Each task inserts its journeys after a named anchor journey, so parallel tasks touch different hunks of `tests/ui_e2e/read.rs`. Task 10 is the visual check.
7. **The concept's title heading is removed from the body,** since the page title shows it (the mockup shows no repeat).
8. **Home reads 50 changes a page** (was 20), the server's default, so a first screen holds a few groups.
9. **Task 0 adds a `worker` gate** to `gate.sh`, so the Node tests have a bounded runner of their own (brief-common: never run tests outside `gate.sh`).

---

## File structure

| File | Task | Responsibility |
|---|---|---|
| `tools/agentic/gate.sh`, `tools/agentic/tests/run.sh`, `tools/agentic/README.md`, `tools/agentic/brief-common.md` (modify) | 0 | The `worker` gate (`just test-worker`, 5 min, no lock). |
| `src/mcp/documents.rs` (modify) | 1 | `text_field` becomes `pub(crate)`, for the browse routes. |
| `src/http/api/browse.rs` (modify) | 1 | `TreeDocument.description`; `RecentChange.title` and `.description` from the snapshot (null when deleted or missing). |
| `src/render.rs` (modify) | 1 | `to_text`: markdown reduced to plain text by the same parser and options as `to_html`. |
| `src/http/api/search.rs` (modify) | 1 | `GET /search` answers `WebSearch`: each hit plus `snippet_text`. |
| `src/http/ui.rs` (modify) | 1, 2 | Task 1: `font-src 'self'` in the shared CSP. Task 2: `txt` content type. |
| `docs/operations.md` (modify) | 1 | The API table rows for tree, recent-changes and search. |
| `scripts/subset-reader-fonts.sh`, `scripts/reader-fonts.sha256` (create) | 2 | Builds and records the Latin-subset woff2 fonts. |
| `src/http/read/fonts/{inter-latin,literata-latin,literata-italic-latin}.woff2`, `OFL-Inter.txt`, `OFL-Literata.txt` (create) | 2 | The self-hosted fonts and their licenses (embedded, served, precached). |
| `src/http/read/icons.svg` (create) | 2 | The icon sprite: one `<symbol id="i-NAME">` per icon. |
| `src/http/read/read.css` (rewrite) | 2 | Tokens (light, dark, `data-theme`), Pico mapping, `@font-face`, chrome, shared parts, one section per screen. |
| `src/http/read/manifest.webmanifest`, `src/http/read/index.html` (modify) | 2, 4, 9 | Task 2: neutral manifest colours and per-scheme `theme-color`. Task 4: header and tab markup with icons. Task 9: `theme.js`. |
| `src/http/read.rs` (modify) | 2, 4 | Tests: the shell lists fonts, sprite and licenses (2); every icon the reader names is in the sprite (4). |
| `src/http/read/js/text.js`, `timeline.js`, `outline.js`, `backlinks.js`, `avatar.js` (create) | 3 | Pure display logic, no DOM: labels, ages, counts; timeline grouping and filters; the "On this page" headings; backlink rows; avatar initials and colour. |
| `tests/read_worker/{text,timeline,outline,backlinks,avatar}.test.mjs` (create) | 3 | Their Node tests. |
| `src/http/read/js/icons.js` (create) | 4 | `icon(name, cls)`: an `<svg><use href="/read/icons.svg#i-NAME">`. |
| `src/http/read/js/parts.js` (modify) | 4 | Shared parts: `searchForm` (icon, placeholder), `crumbs` (catalog label, folder titles), `renderBody` (serif), `row`, `sectionHead`, `pill`, `pageTitle`, `lede`, `message`. Frozen after Task 4. |
| `src/http/read/js/main.js` (modify) | 4 | Catalog list (labels, page counts), header label and mark, scroll reset, header hairline, new `ctx` members. |
| `tests/read_worker/tree-doubles/{parts,icons}.mjs`, `tests/read_worker/esm-hooks.mjs` (modify, create) | 4 | Doubles of the new parts and of `icons.js`. |
| `src/http/read/js/screens/home.js` (rewrite) | 5 | Search box with page count, Jump back in, activity timeline, offline list. |
| `src/http/read/js/listing.js`, `screens/tree.js`, `screens/document.js` (modify) | 6 | Folder listing with titles, descriptions, counts and log; the folder screen; About and Log titles. |
| `tests/read_worker/route.test.mjs`, `tree.test.mjs` (modify) | 6 | Listing and folder screen tests. |
| `src/http/read/js/screens/concept.js` (rewrite) | 7 | Meta row, lede, On this page, serif body, always `?full=true`, deduplicated backlinks. |
| `src/http/read/js/screens/search.js` (rewrite) | 8 | Plain snippets, Show more, `50+ results`. |
| `src/http/read/js/theme.js` (create), `screens/me.js`, `screens/catalogs.js` (modify) | 9 | Theme choice; Account and picker restyle. |
| `tests/ui_e2e/read.rs` (modify) | 4-10 | Journeys: updated where markup changes, new ones per task, the visual-check server (Task 10). |
| `docs/reference/reader.md` (modify), `changelog/<ts>-reader-polish.md` (create) | 11 | Reader reference and the milestone changelog. |

---

## Task map: dependencies and parallel safety

| Task | Sub-issue | Depends on | Runs in parallel with | Shared files (conflict notes) |
|---|---|---|---|---|
| 0 | `worker` gate | none | 1, 2 | `tools/agentic/` only |
| 1 | Server fields and CSP | none | 0, 2, 3 | `src/http/ui.rs`: Task 1 edits the CSP constant and adds a test at the end of `tests`; Task 2 edits `content_type` and its existing test. Different hunks. |
| 2 | Fonts, sprite, design tokens, theme colour | none | 0, 1, 3 | `src/http/read.rs` tests (2 adds after `the_manifest_and_icons_have_their_content_types`; 4 adds at the end). `index.html` (2 edits the head metas only). |
| 3 | Pure modules | 0 | 1, 2 | New files only. |
| 4 | App chrome | 2, 3 | none (it freezes `parts.js`, `main.js`, the doubles and the journey helpers) | `index.html` body, `parts.js`, `main.js`, `read.rs` journeys helpers |
| 5 | Home | 1, 3, 4 | 6, 7, 8, 9 | `read.rs` journeys: home journeys, `HOME_HINT`, inserts after `home_lists_cached_recent_changes_and_viewed_concepts_offline` |
| 6 | Folder | 1, 4 | 5, 7, 8, 9 | `read.rs`: picker/tree journey, log journeys' `#document-title` lines, the two folder-index journeys, inserts after `the_picker_and_the_tree_lead_from_a_catalog_to_its_concepts` |
| 7 | Concept | 3, 4 | 5, 6, 8, 9 | `read.rs`: the `SHORT_OUTLINE` journeys, the `?full=true` suffixes, inserts after `a_concept_shows_its_body_follows_links_and_lists_backlinks` |
| 8 | Search | 1, 4 | 5, 6, 7, 9 | `read.rs`: inserts after `home_and_search_without_a_connection` |
| 9 | Account, picker, theme | 4 | 5, 6, 7, 8 | `index.html` head (one `<script>` line), `read.rs`: inserts after `an_account_without_an_email_shows_no_null` |
| 10 | Visual check | 5, 6, 7, 8, 9 | none | `read.rs` (end of file) |
| 11 | Docs and milestone changelog | 10 | none | `docs/reference/reader.md` |

`read.css` is written whole in Task 2 with one section per screen; Tasks 5 to 9 change only their own section if they must, so they do not conflict. `parts.js`, `main.js` and the doubles are frozen after Task 4: a screen task that needs a new shared part adds it inside its own screen file instead.

---
### Task 0: A `worker` gate for the reader's Node tests

**Files:**
- Modify: `tools/agentic/gate.sh` (header comment, usage line, the `case`)
- Modify: `tools/agentic/tests/run.sh` (a test after `test_gate_dylint_timeout_returns_124`)
- Modify: `tools/agentic/README.md` (the `gate.sh` row and the `AGENTIC_TIMEOUT_<GATE>` row)
- Modify: `tools/agentic/brief-common.md` (the gates block)

- [ ] **Step 1: Write the failing shell test**

In `tools/agentic/tests/run.sh`, after `test_gate_dylint_timeout_returns_124`:

```bash
test_gate_worker_runs_the_just_recipe() {
    new_fixtures; fake_just 'printf "%s\n" "$*" >"$GH_FIXTURES/just_args"; exit 0'
    out=$(cd "$repo" && PATH="$fx/bin:$PATH" "$tools/gate.sh" worker 2>&1)
    rc=$?
    want_rc 0
    [ "$(cat "$fx/just_args")" = test-worker ] \
        || die "just got '$(cat "$fx/just_args")', want 'test-worker'"
}

test_gate_worker_timeout_returns_124() {
    new_fixtures; fake_just 'exec sleep 30'
    out=$(cd "$repo" && PATH="$fx/bin:$PATH" AGENTIC_TIMEOUT_WORKER=1s \
        "$tools/gate.sh" worker 2>&1)
    rc=$?
    want_rc 124; want_out "Treat this as a HANG"
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `just test-agentic`
Expected: `FAIL test_gate_worker_runs_the_just_recipe` (gate.sh exits 2, "unknown gate 'worker'"), and the same for the timeout test.

- [ ] **Step 3: Add the gate**

In `tools/agentic/gate.sh`, add to the header's gate list, after the `dylint` line:

```bash
#   worker   just test-worker             5m   (no lock; the reader's Node tests, also run by test)
```

Change the usage line to:

```bash
gate=${1:?usage: gate.sh fmt|lint|test|test-ui|dylint|worker|narrow [args...]}
```

and add to the `case`, after `dylint)`:

```bash
    worker)  limit=${AGENTIC_TIMEOUT_WORKER:-5m};  locked=0; cmd=(just test-worker "$@") ;;
```

- [ ] **Step 4: Run it to see it pass**

Run: `just test-agentic`
Expected: every test `ok`, last line `N passed, 0 failed`.

- [ ] **Step 5: Document it**

In `tools/agentic/README.md`, the `gate.sh` row: change "Runs `fmt`, `lint`, `test`, `test-ui`, `dylint` (...) or a narrow `cargo test`" to "Runs `fmt`, `lint`, `test`, `test-ui`, `dylint` (...), `worker` (the reader's Node tests) or a narrow `cargo test`". In the configuration table, the `AGENTIC_TIMEOUT_<GATE>` row becomes `5m / 30m / 75m / 60m / 45m / 5m / 15m` for "fmt, lint, test, test-ui, dylint, worker, narrow". In `tools/agentic/brief-common.md`'s gates block, after the `dylint` line:

```
tools/agentic/gate.sh worker               # the reader's Node tests (tests/read_worker/), while iterating
```

- [ ] **Step 6: Changelog and commit**

Write `changelog/<ts>-reader-polish-worker-gate.md`:

```markdown
# Agentic tools: a gate for the reader's Node tests

## Added
- `tools/agentic/gate.sh worker` runs `just test-worker` (the mobile reader's
  Node tests) under a 5-minute limit (`AGENTIC_TIMEOUT_WORKER`), without the
  shared lock, so agents iterating on the reader's modules have a bounded
  runner. `just test` still runs them too.

## Tests
- `tools/agentic/tests/run.sh`: the gate runs the recipe, and a hang exits 124.
```

```bash
git add tools/agentic changelog
git commit -m "tools(agentic): a worker gate for the reader's Node tests

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 1: Server additions: titles, descriptions, plain snippets, own fonts

**Files:**
- Modify: `src/mcp/documents.rs:167` (`text_field` visibility)
- Modify: `src/http/api/browse.rs` (`TreeDocument`, `tree`, `RecentChange`, `recent_changes`, tests)
- Modify: `src/render.rs` (add `to_text` after `to_html`, tests)
- Modify: `src/http/api/search.rs` (answer type, tests)
- Modify: `src/http/ui.rs:18-21` (CSP) and its tests
- Modify: `docs/operations.md:3879,3885,3886`

All four changes are additive: existing fields are unchanged, so the console keeps working.

- [ ] **Step 1: Write the failing tree test**

In `src/http/api/browse.rs` tests, change the exact entry in `the_tree_lists_concepts_indexes_the_log_and_agents_with_titles` to:

```rust
        assert_eq!(
            entry(&tree, "/systems/alpha.md"),
            &json!({"path": "/systems/alpha.md", "kind": "concept", "title": "Alpha", "description": null})
        );
```

and add after that test:

```rust
    #[tokio::test]
    async fn the_tree_gives_each_document_its_description() {
        let test = app_with(
            &[
                (
                    "main",
                    &[
                        ("index.md", "---\ntitle: Main\ndescription: Everything we keep.\n---\n# Main\n"),
                        ("systems/alpha.md", "---\ntype: system\ntitle: Alpha\ndescription: Runs the jobs.\n---\n# Alpha\n"),
                        ("systems/beta.md", "---\ntype: system\ntitle: Beta\n---\n# Beta\n"),
                        ("systems/gamma.md", "---\ntitle: Gamma\ndescription: [not, a, string]\n---\n# Gamma\n"),
                    ],
                ),
                ("other", &[("index.md", "# Other\n")]),
            ],
            Config::default(),
        )
        .await;
        let keys = keys(&test).await;

        let (status, tree) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/tree",
            Some(&keys.read),
            None,
        )
        .await;

        assert_eq!(status.as_u16(), 200, "{tree}");
        assert_eq!(entry(&tree, "/index.md")["description"], "Everything we keep.");
        assert_eq!(entry(&tree, "/systems/alpha.md")["description"], "Runs the jobs.");
        // No description, or one that is not a string: null.
        assert_eq!(entry(&tree, "/systems/beta.md")["description"], Value::Null);
        assert_eq!(entry(&tree, "/systems/gamma.md")["description"], Value::Null);
    }
```

- [ ] **Step 2: Run it to see it fail**

Run (with `TEST_PG_URL` set as in "Rules"): `tools/agentic/gate.sh narrow --lib -- http::api::browse::tests::the_tree`
Expected: both tests FAIL: the entries have no `description` key.

- [ ] **Step 3: Add the tree's description**

In `src/mcp/documents.rs`, make the helper reachable from the browse routes (only its visibility changes):

```rust
pub(crate) fn text_field(doc: &SnapshotDoc, key: &str) -> Option<String> {
```

In `src/http/api/browse.rs`, extend the import:

```rust
use crate::mcp::documents::{
    get_concept, read_document, text_field, Concept, Document, GetConceptParams, InboundLink,
    OutboundLink, ReadDocumentParams,
};
```

add the field to `TreeDocument` after `title`:

```rust
    /// Its frontmatter `description`, when that is a string.
    pub description: Option<String>,
```

and fill it in `tree`:

```rust
            Some(TreeDocument {
                path: format!("/{}", doc.path()),
                kind: kind_name(doc.kind(), doc.path())?,
                title: doc.title().map(str::to_string),
                description: text_field(doc, "description"),
            })
```

- [ ] **Step 4: Run it to see it pass**

Run: `tools/agentic/gate.sh narrow --lib -- http::api::browse::tests::the_tree`
Expected: PASS (2 tests).

- [ ] **Step 5: Write the failing recent-changes test**

Add after `recent_changes_list_one_catalogs_published_revisions_newest_first`:

```rust
    /// Each change names its document as the catalog has it now: the title
    /// and description of the current file, and null for a deletion or a
    /// document that is gone since.
    #[tokio::test]
    async fn recent_changes_carry_titles_and_descriptions_from_the_snapshot() {
        let test = app().await;
        let keys = keys(&test).await;
        let alpha = "---\ntype: system\ntitle: Alpha\ndescription: Runs the jobs.\n---\n# Alpha\n";
        let entries: Vec<(&str, &str)> = MAIN
            .iter()
            .filter(|(path, _)| *path != "systems/beta.md")
            .map(|(path, text)| {
                if *path == "systems/alpha.md" {
                    (*path, alpha)
                } else {
                    (*path, *text)
                }
            })
            .collect();
        publish(&test, "main", &entries, "editor").await;

        let (status, page) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/recent-changes",
            Some(&keys.read),
            None,
        )
        .await;

        assert_eq!(status.as_u16(), 200, "{page}");
        let change = |path: &str, revision: i64| -> Value {
            page["changes"]
                .as_array()
                .expect("changes")
                .iter()
                .find(|change| change["path"] == path && change["revision"] == revision)
                .cloned()
                .unwrap_or_else(|| panic!("{path} revision {revision} in {page}"))
        };
        let edited = change("/systems/alpha.md", 2);
        assert_eq!(edited["title"], "Alpha");
        assert_eq!(edited["description"], "Runs the jobs.");
        // An older revision of a file that still exists names it as it is now.
        assert_eq!(change("/systems/alpha.md", 1)["description"], "Runs the jobs.");
        // The deletion, and the earlier revision of the deleted file: null.
        for revision in [1, 2] {
            let gone = change("/systems/beta.md", revision);
            assert_eq!(gone["title"], Value::Null, "{gone}");
            assert_eq!(gone["description"], Value::Null, "{gone}");
        }
        // A file with no title: null, not its path.
        assert_eq!(change("/log.md", 1)["title"], Value::Null);
    }
```

- [ ] **Step 6: Run it to see it fail**

Run: `tools/agentic/gate.sh narrow --lib -- http::api::browse::tests::recent_changes_carry`
Expected: FAIL: `edited["title"]` is null (no such key).

- [ ] **Step 7: Add title and description to recent changes**

Add to `RecentChange`, after `path`:

```rust
    /// The document's title as the catalog has it now; `None` for a
    /// deletion, a document gone since, or one without a title.
    pub title: Option<String>,
    /// Its frontmatter `description` now, when that is a string; `None` as
    /// for `title`.
    pub description: Option<String>,
```

In `recent_changes`, replace the `let changes = page ... .collect();` statement with:

```rust
    // Titles and descriptions come from the published snapshot, read now:
    // the history rows hold no content.
    let loaded = ctx.snapshot(&state);
    let snapshot = loaded.snapshot();
    let changes = page
        .rows
        .into_iter()
        .map(|row| {
            // A deletion names no document, even if one was created again
            // at the path since.
            let doc = if row.op == "delete" {
                None
            } else {
                snapshot.get(&row.path)
            };
            RecentChange {
                path: format!("/{}", row.path),
                title: doc.and_then(|doc| doc.title().map(str::to_string)),
                description: doc.and_then(|doc| text_field(doc, "description")),
                revision: row.revision,
                op: row.op,
                actor_label: labels.actor(&row.actor).map(str::to_string),
                actor: row.actor,
                message: row.message,
                at: format_timestamp(row.created_at),
            }
        })
        .collect();
```

- [ ] **Step 8: Run the browse tests**

Run: `tools/agentic/gate.sh narrow --lib -- http::api::browse`
Expected: PASS (every browse test, the three new or changed ones included).

- [ ] **Step 9: Write the failing `to_text` tests**

In `src/render.rs`'s test module, add:

```rust
    #[test]
    fn to_text_keeps_only_the_text_of_inline_markdown() {
        assert_eq!(
            to_text("Alpha runs [Beta](beta.md) with **two** `keys` and _one_ ~~old~~ note."),
            "Alpha runs Beta with two keys and one old note."
        );
        // An image keeps its alt text; raw HTML stays text, as to_html shows it.
        assert_eq!(to_text("See ![the map](map.png) <b>now</b>"), "See the map <b>now</b>");
    }

    #[test]
    fn to_text_turns_block_structure_into_single_spaces() {
        assert_eq!(to_text("## Config"), "Config");
        assert_eq!(to_text("- one\n- two"), "one two");
        assert_eq!(to_text("> quoted\nline"), "quoted line");
        assert_eq!(to_text("first\n\n\nsecond   third"), "first second third");
        assert_eq!(to_text(""), "");
    }
```

Run: `tools/agentic/gate.sh narrow --lib -- render::tests::to_text`
Expected: FAIL to compile: `cannot find function to_text`.

- [ ] **Step 10: Write `to_text`**

In `src/render.rs`, after `to_html`:

```rust
/// Markdown reduced to plain text, for a one-line preview such as a search
/// snippet: the text the rendered HTML would show, with no markup and no
/// markdown syntax. It parses with the same options as [`to_html`]. Links
/// keep their text, images their alt text, code its content, and raw HTML
/// stays text (as [`to_html`] shows it); line breaks and the ends of
/// paragraphs, headings, list items, quotes and table cells become spaces,
/// and every run of whitespace becomes one space.
///
/// # Arguments
/// * `markdown` - a line or a body; frontmatter is not split off.
///
/// # Returns
/// The text, trimmed.
///
/// # Examples
/// ```
/// use okf_mcp::render::to_text;
/// assert_eq!(to_text("Runs [Beta](beta.md) with **two** `keys`."), "Runs Beta with two keys.");
/// ```
#[tracing::instrument(level = "trace", skip_all)]
pub fn to_text(markdown: &str) -> String {
    let mut text = String::with_capacity(markdown.len());
    for event in Parser::new_ext(markdown, options()) {
        match event {
            Event::Text(part) | Event::Code(part) | Event::Html(part) | Event::InlineHtml(part) => {
                text.push_str(&part);
            }
            Event::SoftBreak
            | Event::HardBreak
            | Event::Rule
            | Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::Item
                | TagEnd::BlockQuote(_)
                | TagEnd::CodeBlock
                | TagEnd::TableCell,
            ) => text.push(' '),
            // Start tags, other end tags, footnote references, task list
            // markers and math add no text of their own. A catch-all,
            // because pulldown-cmark's `Event` has many more variants and
            // grows with its versions.
            _ => {}
        }
    }
    // `split_whitespace` drops every run of whitespace, so joining with one
    // space collapses them and trims both ends.
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
```

Run: `tools/agentic/gate.sh narrow --lib -- render::tests::to_text`
Expected: PASS (2 tests). Then `tools/agentic/gate.sh narrow --doc -- render::to_text`: PASS.

- [ ] **Step 11: Write the failing search test**

In `src/http/api/search.rs`, in `search_finds_a_concept_with_its_catalog_and_snippet`, after the `snippet` assertion:

```rust
        // The same snippet without its markdown syntax, for the reader.
        assert_eq!(found["hits"][0]["snippet_text"], "Alpha runs Beta.");
        // An empty query has no snippet, so no plain one either.
        assert_eq!(limited["hits"][0]["snippet_text"], serde_json::Value::Null);
```

Run: `tools/agentic/gate.sh narrow --lib -- http::api::search::tests::search_finds`
Expected: FAIL: `snippet_text` is absent (`Null` against `"Alpha runs Beta."`).

- [ ] **Step 12: Answer with plain snippets**

In `src/http/api/search.rs`, change the imports:

```rust
use serde::{Deserialize, Serialize};

use crate::mcp::search::{run, LabeledHit, SearchParams, SearchResult, SkippedCatalog};
use crate::render::to_text;
```

Add after `SearchArgs`:

```rust
/// One hit as `GET /search` answers it: the `search` tool's hit, plus its
/// snippet as plain text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WebHit {
    /// The tool's hit; `flatten` writes its fields beside `snippet_text`.
    #[serde(flatten)]
    pub hit: LabeledHit,
    /// `snippet` with its markdown syntax removed ([`to_text`]), for a
    /// client that shows it as text (the mobile reader); null without a
    /// snippet.
    pub snippet_text: Option<String>,
}

/// `GET /search`'s answer: the tool's result, each hit a [`WebHit`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WebSearch {
    /// As [`SearchResult::hits`], best first.
    pub hits: Vec<WebHit>,
    /// As [`SearchResult::skipped`].
    pub skipped: Vec<SkippedCatalog>,
}

/// The tool's result with each snippet also as plain text.
///
/// # Arguments
/// * `result` - the `search` tool's result.
///
/// # Returns
/// The same hits, in the same order, each with `snippet_text`.
#[tracing::instrument(level = "trace", skip_all)]
fn with_plain_snippets(result: SearchResult) -> WebSearch {
    let hits = result
        .hits
        .into_iter()
        .map(|hit| {
            // `hit.hit.hit` is the LabeledHit's CatalogHit's SearchHit.
            let snippet_text = hit.hit.hit.snippet.as_deref().map(to_text);
            WebHit { hit, snippet_text }
        })
        .collect();
    WebSearch {
        hits,
        skipped: result.skipped,
    }
}
```

and in `search`, change the return type to `Result<Json<WebSearch>, AppError>` and the last line to:

```rust
    Ok(Json(with_plain_snippets(result)))
```

Each snippet is at most 160 characters and a call returns at most 200 hits, so this stays on the async thread.

Run: `tools/agentic/gate.sh narrow --lib -- http::api::search`
Expected: PASS.

- [ ] **Step 13: Write the failing CSP test**

The CSP is one constant shared by the console, the sign-in pages and the reader (`src/http/ui.rs`, used by `read.rs` and `signin.rs`). `'self'` in `font-src` lets the reader load its own fonts and changes nothing for the console, which loads none from its origin. Add at the end of `src/http/ui.rs`'s tests:

```rust
    /// The reader's self-hosted fonts load under the shared policy, which
    /// still allows Google Fonts for the console.
    #[tokio::test]
    async fn the_policy_lets_pages_load_fonts_from_their_own_origin() {
        for path in ["/ui/", "/read/"] {
            let response = get(path).await;
            let policy = response.headers()["content-security-policy"]
                .to_str()
                .expect("an ASCII header");
            assert!(
                policy.contains("font-src 'self' https://fonts.gstatic.com;"),
                "{path}: {policy}"
            );
        }
    }
```

Run: `tools/agentic/gate.sh narrow --lib -- http::ui::tests::the_policy`
Expected: FAIL: the policy has `font-src https://fonts.gstatic.com;`.

- [ ] **Step 14: Allow the origin's fonts**

```rust
/// The console's Content-Security-Policy, shared by the sign-in pages and
/// the reader: scripts and styles only from this origin (Google Fonts'
/// stylesheet excepted), fonts from this origin (the reader's) or Google
/// Fonts (the console's), no framing.
pub(crate) const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; \
    style-src 'self' https://fonts.googleapis.com; font-src 'self' https://fonts.gstatic.com; \
    img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; \
    form-action 'self'";
```

Run: `tools/agentic/gate.sh narrow --lib -- http::ui` then `tools/agentic/gate.sh narrow --lib -- http::read` then `tools/agentic/gate.sh narrow --lib -- http::signin`
Expected: PASS (the other tests compare against the constant, so they follow it).

- [ ] **Step 15: Document the API**

In `docs/operations.md`:
- line 3879 (tree): "`path`, `kind`, `title`" becomes "`path`, `kind`, `title`, `description` (the frontmatter's, when it is a string; else null)".
- line 3885 (recent-changes): "each change `{path, revision, op, actor, actor_label, message, at}`" becomes "each change `{path, title, description, revision, op, actor, actor_label, message, at}` (`title` and `description` as the catalog has the document now; null for a deletion or a document gone since)".
- line 3886 (search): append "Each hit also has `snippet_text`, the snippet with its markdown syntax removed (null without a snippet)."

- [ ] **Step 16: Gates**

Run: `tools/agentic/gate.sh fmt`, `tools/agentic/gate.sh lint`, `tools/agentic/gate.sh test`, `tools/agentic/gate.sh dylint`
Expected: each exits 0. (`test-ui` too if time allows: the console journeys read these routes, and only gain fields.)

- [ ] **Step 17: Changelog and commit**

`changelog/<ts>-reader-polish-server-fields.md`:

```markdown
# Browse and search: titles, descriptions and plain snippets for the reader

## Added
- `GET /api/v1/catalogs/{c}/recent-changes`: each change has `title` and
  `description`, read from the published snapshot at request time; both are
  null for a deletion or a document that is gone since.
- `GET /api/v1/catalogs/{c}/tree`: each document has `description`.
- `GET /api/v1/search`: each hit has `snippet_text`, the snippet with its
  markdown syntax removed (`render::to_text`, the console renderer's parser
  reduced to text). `snippet` is unchanged.

## Changed
- The shared Content-Security-Policy allows fonts from the server's own
  origin (`font-src 'self' https://fonts.gstatic.com`), for the reader's
  self-hosted fonts.

## Tests
- Tree descriptions; recent-changes titles and descriptions, null for a
  deletion; `to_text`; `snippet_text`; the policy on `/ui/` and `/read/`.

## Docs
- `docs/operations.md`'s API table.
```

```bash
git add src docs/operations.md changelog
git commit -m "feat(api): titles and descriptions in recent changes and the tree, plain search snippets, own-origin fonts

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 2: Fonts, icon sprite, design tokens and the system-bar colour

**Files:**
- Create: `scripts/subset-reader-fonts.sh`, `scripts/reader-fonts.sha256`
- Create: `src/http/read/fonts/inter-latin.woff2`, `literata-latin.woff2`, `literata-italic-latin.woff2`, `OFL-Inter.txt`, `OFL-Literata.txt`
- Create: `src/http/read/icons.svg`
- Rewrite: `src/http/read/read.css`
- Modify: `src/http/read/manifest.webmanifest`, `src/http/read/index.html` (head metas only)
- Modify: `src/http/ui.rs` (`content_type`: `txt`; its test)
- Modify: `src/http/read.rs` (a shell test)

Every file under `src/http/read/` is embedded by `build.rs` and listed, with its SHA-256, in `/read/shell.json`, so the worker precaches the fonts, the sprite and the licenses with no change to `read.rs` or `sw.js`. The screens adopt the new classes in Tasks 4 to 9; until then the integration branch shows the old markup with the new base styles, which is expected.

- [ ] **Step 1: Write the failing shell test**

In `src/http/read.rs` tests, after `the_manifest_and_icons_have_their_content_types`:

```rust
    #[tokio::test]
    async fn the_shell_precaches_the_fonts_the_sprite_and_their_licenses() {
        let response = get("/read/shell.json").await;
        let listing: serde_json::Value = serde_json::from_str(&text(response).await).expect("json");
        let files: Vec<&str> = listing["files"]
            .as_array()
            .expect("files")
            .iter()
            .filter_map(|entry| entry["url"].as_str())
            .collect();
        for url in [
            "/read/icons.svg",
            "/read/fonts/inter-latin.woff2",
            "/read/fonts/literata-latin.woff2",
            "/read/fonts/literata-italic-latin.woff2",
            "/read/fonts/OFL-Inter.txt",
            "/read/fonts/OFL-Literata.txt",
        ] {
            assert!(files.contains(&url), "{url} in {files:?}");
        }
        assert_eq!(
            get("/read/fonts/inter-latin.woff2").await.headers()["content-type"],
            "font/woff2"
        );
        assert_eq!(get("/read/icons.svg").await.headers()["content-type"], "image/svg+xml");
        assert_eq!(
            get("/read/fonts/OFL-Inter.txt").await.headers()["content-type"],
            "text/plain; charset=utf-8"
        );
    }

    #[tokio::test]
    async fn the_system_bars_follow_the_theme() {
        let page = text(get("/read/").await).await;
        assert!(page.contains(r##"<meta name="theme-color" content="#ffffff" media="(prefers-color-scheme: light)">"##));
        assert!(page.contains(r##"<meta name="theme-color" content="#191919" media="(prefers-color-scheme: dark)">"##));
        let manifest: serde_json::Value =
            serde_json::from_str(&text(get("/read/manifest.webmanifest").await).await).expect("json");
        // Neutral, so no colour of the old accent shows in a system bar.
        assert_eq!(manifest["theme_color"], "#ffffff");
        assert_eq!(manifest["background_color"], "#ffffff");
    }
```

The existing `the_shell_listing_names_every_reader_file_and_the_shared_ones` already fetches every listed file and checks its hash, so the new files are covered by it too.

In `src/http/ui.rs`'s `content_types_follow_the_extension`, add:

```rust
        assert_eq!(content_type("fonts/OFL.txt"), "text/plain; charset=utf-8");
```

- [ ] **Step 2: Run them to see them fail**

Run: `tools/agentic/gate.sh narrow --lib -- http::read::tests::the_s` and `tools/agentic/gate.sh narrow --lib -- http::ui::tests::content_types`
Expected: FAIL (no such files; the metas still carry the old colours; `txt` is `application/octet-stream`).

- [ ] **Step 3: The `txt` content type**

In `src/http/ui.rs` `content_type`, after the `webmanifest` arm:

```rust
        Some("txt") => "text/plain; charset=utf-8",
```

- [ ] **Step 4: Write the font script**

`scripts/subset-reader-fonts.sh` (make it executable, `chmod +x`):

```bash
#!/usr/bin/env bash
# Builds the reader's self-hosted fonts in src/http/read/fonts/ (reader
# polish design, "Type"): Latin-subset woff2 of Inter (the interface) and
# Literata (concept bodies, roman and italic), both under the SIL Open Font
# License, with their license files beside them. Run it once, when a font is
# added or upgraded, and commit what it writes: the build only embeds them.
#
#   scripts/subset-reader-fonts.sh INTER_DIR LITERATA_DIR
#
# INTER_DIR     the unpacked Inter 4.1 release archive (Inter-4.1.zip from
#               the Inter project's releases), holding InterVariable.ttf
#               and LICENSE.txt.
# LITERATA_DIR  the unpacked Literata family download from Google Fonts,
#               holding Literata-VariableFont_opsz,wght.ttf,
#               Literata-Italic-VariableFont_opsz,wght.ttf and OFL.txt.
#
# It pins the axes the reader uses (Inter 400 to 700, Literata 400 to 600,
# both at their default optical size), keeps the Latin range only, refuses a
# file over 150 KB, and records each output's SHA-256 in
# scripts/reader-fonts.sha256 (check later with `sha256sum -c` from the
# repository root). uvx runs a pinned fonttools; the system Python is never
# used.
set -euo pipefail

readonly FONTTOOLS='fonttools[woff]==4.55.3'
# The "latin" range Google Fonts serves.
readonly LATIN='U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,U+2000-206F,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215,U+FEFF,U+FFFD'
readonly MAX_BYTES=153600
readonly OUT=src/http/read/fonts
readonly SUMS=scripts/reader-fonts.sha256

[ $# -eq 2 ] || { echo "usage: $0 INTER_DIR LITERATA_DIR" >&2; exit 2; }
command -v uvx >/dev/null || { echo "$0: needs uv (uvx)" >&2; exit 2; }
[ -f build.rs ] || { echo "$0: run it from the repository root" >&2; exit 2; }
inter_dir=$1
literata_dir=$2
inter=$inter_dir/InterVariable.ttf
literata=$literata_dir/Literata-VariableFont_opsz,wght.ttf
literata_italic=$literata_dir/Literata-Italic-VariableFont_opsz,wght.ttf
for file in "$inter" "$inter_dir/LICENSE.txt" "$literata" "$literata_italic" "$literata_dir/OFL.txt"; do
    [ -f "$file" ] || { echo "$0: missing $file" >&2; exit 2; }
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$OUT"

# build INPUT OUTPUT AXIS...: pins INPUT's axes as given, keeps the Latin
# range, and writes OUTPUT as woff2.
build() {
    local input=$1 output=$2
    shift 2
    uvx --from "$FONTTOOLS" fonttools varLib.instancer "$input" "$@" --output "$work/pinned.ttf"
    uvx --from "$FONTTOOLS" pyftsubset "$work/pinned.ttf" --unicodes="$LATIN" \
        --flavor=woff2 --output-file="$output"
    local size
    size=$(stat -c %s "$output")
    if [ "$size" -gt "$MAX_BYTES" ]; then
        echo "$0: $output is $size bytes, over $MAX_BYTES" >&2
        exit 1
    fi
    echo "$output: $size bytes"
}

build "$inter" "$OUT/inter-latin.woff2" wght=400:700 opsz=drop
build "$literata" "$OUT/literata-latin.woff2" wght=400:600 opsz=drop
build "$literata_italic" "$OUT/literata-italic-latin.woff2" wght=400:600 opsz=drop
cp "$inter_dir/LICENSE.txt" "$OUT/OFL-Inter.txt"
cp "$literata_dir/OFL.txt" "$OUT/OFL-Literata.txt"
sha256sum "$OUT"/*.woff2 "$OUT"/OFL-*.txt >"$SUMS"
echo "inputs:"
sha256sum "$inter" "$literata" "$literata_italic"
```

- [ ] **Step 5: Build the fonts**

Download the two releases by hand into a new, empty directory in the session scratchpad (never into the repository), unpack them there, and run from the repository root:

```bash
scripts/subset-reader-fonts.sh "$SCRATCH/fonts/Inter-4.1" "$SCRATCH/fonts/Literata"
```

Expected: three lines `src/http/read/fonts/<name>.woff2: <N> bytes`, each under 153600. Estimates: Inter about 70 to 95 KB, each Literata file about 60 to 95 KB, so about 250 KB precached in all. Then the inputs' SHA-256. Copy the input hashes into this task's changelog entry. Check: `sha256sum -c scripts/reader-fonts.sha256` prints `OK` for all five files. Open each `OFL-*.txt` and check it is the SIL Open Font License 1.1 text with the font's copyright line.

- [ ] **Step 6: Write the icon sprite**

`src/http/read/icons.svg` (the mockup's symbols the reader uses):

```xml
<svg xmlns="http://www.w3.org/2000/svg">
  <!-- The reader's icons (reader polish design, "Icons"), drawn as
       <svg class="icon"><use href="/read/icons.svg#i-NAME"/></svg> by
       js/icons.js. Strokes take the page's colour (read.css .icon). -->
  <symbol id="i-home" viewBox="0 0 24 24"><path d="M3 10.5 12 3l9 7.5"/><path d="M5 9.5V21h14V9.5"/><path d="M10 21v-6h4v6"/></symbol>
  <symbol id="i-folder" viewBox="0 0 24 24"><path d="M3 6.5A1.5 1.5 0 0 1 4.5 5H9l2 2.5h8.5A1.5 1.5 0 0 1 21 9v9.5a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z"/></symbol>
  <symbol id="i-doc" viewBox="0 0 24 24"><path d="M6 3h8l4 4v14H6z"/><path d="M14 3v4h4"/><path d="M9 12h6M9 16h6"/></symbol>
  <symbol id="i-search" viewBox="0 0 24 24"><circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/></symbol>
  <symbol id="i-user" viewBox="0 0 24 24"><circle cx="12" cy="8" r="4"/><path d="M4 21c1.5-4 4.5-6 8-6s6.5 2 8 6"/></symbol>
  <symbol id="i-chev" viewBox="0 0 24 24"><path d="m9 6 6 6-6 6"/></symbol>
  <symbol id="i-down" viewBox="0 0 24 24"><path d="m6 9 6 6 6-6"/></symbol>
  <symbol id="i-list" viewBox="0 0 24 24"><path d="M8 6h13M8 12h13M8 18h13M3.5 6h.01M3.5 12h.01M3.5 18h.01"/></symbol>
  <symbol id="i-info" viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="M12 11v5M12 8h.01"/></symbol>
  <symbol id="i-offline" viewBox="0 0 24 24"><path d="M12 3v12M7 10l5 5 5-5"/><path d="M5 21h14"/></symbol>
  <symbol id="i-log" viewBox="0 0 24 24"><path d="M5 4h14v16H5z"/><path d="M9 8h6M9 12h6M9 16h3"/></symbol>
</svg>
```

- [ ] **Step 7: Theme colours in the manifest and the head**

`src/http/read/manifest.webmanifest`: set `"background_color": "#ffffff"` and `"theme_color": "#ffffff"` (the rest unchanged). In `src/http/read/index.html`, the two theme-color metas become:

```html
    <meta name="theme-color" content="#ffffff" media="(prefers-color-scheme: light)">
    <meta name="theme-color" content="#191919" media="(prefers-color-scheme: dark)">
```

(`#ffffff` and `#191919` are `--bg` in each scheme below. Android takes the page's matching meta once it is loaded, so the navigation bar follows the theme; the manifest's neutral colour shows only on the splash.)

- [ ] **Step 8: Rewrite `read.css`**

Replace `src/http/read/read.css` with:

```css
/* The mobile reader's look (reader polish design), on Pico CSS 2
 * (/ui/vendor/pico.min.css), which stays the base layer. Notion-like
 * neutrals; Inter for the interface and Literata for concept bodies, both
 * self-hosted (fonts/). Tokens on :root: light by default, dark from the
 * system, or the Account screen's choice (data-theme on <html>). No inline
 * styles (the CSP forbids them): every variant is a class. Body markup is
 * what src/render.rs writes into body_html. After the shared parts comes one
 * section per screen; a screen's change touches only its own section. */

@font-face {
    font-family: "Inter";
    src: url("/read/fonts/inter-latin.woff2") format("woff2");
    font-weight: 400 700;
    font-style: normal;
    font-display: swap;
}
@font-face {
    font-family: "Literata";
    src: url("/read/fonts/literata-latin.woff2") format("woff2");
    font-weight: 400 600;
    font-style: normal;
    font-display: swap;
}
@font-face {
    font-family: "Literata";
    src: url("/read/fonts/literata-italic-latin.woff2") format("woff2");
    font-weight: 400 600;
    font-style: italic;
    font-display: swap;
}

/* ---------- Tokens ---------- */
:root {
    --bg: #ffffff; --bg-2: #f7f7f5; --bg-3: #efefed;
    --text: #37352f; --muted: #787774; --faint: #a5a29a; --line: #e9e9e7;
    --accent: #2383e2; --accent-soft: #e7f3fe;
    --green: #448361; --green-soft: #edf3ec;
    --red: #d44c47; --red-soft: #fdebec;
    --amber: #cb912f; --amber-soft: #fbf3db;
    --purple: #9065b0; --purple-soft: #f4f0f7;
    --radius: 10px; --tab-h: 58px; --measure: 42.5rem; --touch: 2.75rem;
    --sans: "Inter", system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
    --serif: "Literata", Georgia, "Iowan Old Style", serif;
    --mono: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
    color-scheme: light;
}
@media (prefers-color-scheme: dark) {
    :root:not([data-theme="light"]) {
        --bg: #191919; --bg-2: #202020; --bg-3: #2a2a2a;
        --text: #e3e2e0; --muted: #9b9a97; --faint: #6f6e69; --line: #2f2f2f;
        --accent: #529cca; --accent-soft: #1f2e3a;
        --green: #4dab9a; --green-soft: #1e2e2a;
        --red: #df5452; --red-soft: #3a1f1f;
        --amber: #d9a443; --amber-soft: #352d1c;
        --purple: #9a6dd7; --purple-soft: #2b2333;
        color-scheme: dark;
    }
}
:root[data-theme="dark"] {
    --bg: #191919; --bg-2: #202020; --bg-3: #2a2a2a;
    --text: #e3e2e0; --muted: #9b9a97; --faint: #6f6e69; --line: #2f2f2f;
    --accent: #529cca; --accent-soft: #1f2e3a;
    --green: #4dab9a; --green-soft: #1e2e2a;
    --red: #df5452; --red-soft: #3a1f1f;
    --amber: #d9a443; --amber-soft: #352d1c;
    --purple: #9a6dd7; --purple-soft: #2b2333;
    color-scheme: dark;
}
/* Pico's variables follow the tokens. Pico sets them on
 * :root:not([data-theme=dark]) and on its dark selectors, at specificity
 * (0,2,0) or less; these two selectors cover every case at (0,2,0), and this
 * file loads after Pico, so they win. The values are tokens, so they change
 * with the scheme by themselves. */
:root:not([data-theme="dark"]), :root[data-theme="dark"] {
    --pico-font-family: var(--sans);
    --pico-font-size: 100%;
    --pico-line-height: 1.5;
    --pico-background-color: var(--bg);
    --pico-color: var(--text);
    --pico-muted-color: var(--muted);
    --pico-muted-border-color: var(--line);
    --pico-primary: var(--accent);
    --pico-primary-background: var(--accent);
    --pico-primary-hover-background: var(--accent);
    --pico-primary-border: var(--accent);
    --pico-primary-hover-border: var(--accent);
    --pico-primary-inverse: #ffffff;
    --pico-secondary-background: var(--bg-3);
    --pico-secondary-border: var(--bg-3);
    --pico-secondary-inverse: var(--text);
    --pico-form-element-background-color: var(--bg-2);
    --pico-form-element-border-color: transparent;
    --pico-form-element-color: var(--text);
    --pico-border-radius: var(--radius);
    --pico-spacing: 1rem;
    --pico-code-background-color: var(--bg-3);
    --pico-code-color: var(--text);
}

/* ---------- Base ---------- */
/* An element's own display (a .row is flex) must not undo `hidden`. */
[hidden] { display: none !important; }
html { -webkit-tap-highlight-color: transparent; }
body {
    margin: 0;
    min-height: 100dvh;
    background: var(--bg);
    color: var(--text);
    font: 16px/1.5 var(--sans);
    padding: 0 env(safe-area-inset-right) calc(var(--tab-h) + env(safe-area-inset-bottom)) env(safe-area-inset-left);
}
a { color: inherit; text-decoration: none; }
.icon {
    width: 20px; height: 20px; flex: none;
    stroke: currentColor; fill: none; stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round;
}
.icon-sm { width: 16px; height: 16px; }
.icon-end { margin-left: auto; color: var(--faint); }
.chev { width: 16px; height: 16px; color: var(--faint); }

/* ---------- Chrome: header, banner, tabs ---------- */
.top {
    position: sticky; top: 0; z-index: 5;
    display: flex; align-items: center; gap: 10px;
    padding: calc(10px + env(safe-area-inset-top)) 16px 10px;
    background: color-mix(in srgb, var(--bg) 88%, transparent);
    -webkit-backdrop-filter: blur(12px);
    backdrop-filter: blur(12px);
    border-bottom: 1px solid transparent;
}
.top.scrolled { border-bottom-color: var(--line); }
.catalog { display: flex; align-items: center; gap: 8px; min-width: 0; color: var(--text); font-size: 15px; font-weight: 600; }
.catalog .label { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.catalog .icon { color: var(--muted); }
.mark {
    width: 26px; height: 26px; flex: none; display: grid; place-items: center;
    border-radius: 7px; background: var(--accent); color: #ffffff; font-size: 13px; font-weight: 700;
}
.spacer { flex: 1; }
.iconbtn { width: var(--touch); height: var(--touch); flex: none; display: grid; place-items: center; border-radius: 8px; color: var(--muted); }
.banner { margin: 0; padding: 8px 16px; background: var(--bg-2); color: var(--muted); font-size: 13px; border-bottom: 1px solid var(--line); }
.tabs {
    position: fixed; left: 0; right: 0; bottom: 0; z-index: 6;
    display: grid; grid-template-columns: repeat(4, 1fr);
    height: calc(var(--tab-h) + env(safe-area-inset-bottom));
    padding-bottom: env(safe-area-inset-bottom);
    background: var(--bg); border-top: 1px solid var(--line);
}
.tabs a { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 2px; color: var(--faint); font-size: 11px; font-weight: 500; }
.tabs a .icon { width: 22px; height: 22px; }
.tabs a[aria-current="page"] { color: var(--text); }
.reader { max-width: var(--measure); margin: 0 auto; padding: 4px 16px 24px; }

/* ---------- Shared parts (parts.js) ---------- */
#search-form {
    display: flex; align-items: center; gap: 10px; margin: 8px 0 0; padding: 0 12px;
    border-radius: 10px; background: var(--bg-2); color: var(--muted); box-shadow: none;
}
#search-form input[type="search"] {
    flex: 1; height: auto; margin: 0; padding: 10px 0; border: 0; border-radius: 0;
    background: transparent; box-shadow: none; color: var(--text); font-size: 15px;
}
#search-form .icon { width: 18px; height: 18px; }
.sec { display: flex; align-items: baseline; justify-content: space-between; margin: 22px 2px 6px; }
.sec h2 { margin: 0; color: var(--muted); font-size: 12px; font-weight: 600; letter-spacing: .02em; text-transform: uppercase; }
.rows { display: flex; flex-direction: column; }
.row { display: flex; align-items: center; gap: 12px; min-height: 48px; padding: 9px 6px; border-radius: 8px; color: var(--text); }
a.row:active, button.row:active { background: var(--bg-2); }
button.row { width: 100%; margin: 0; border: 0; background: none; color: var(--text); font: inherit; text-align: left; }
.row .ic { width: 30px; height: 30px; flex: none; display: grid; place-items: center; border-radius: 7px; background: var(--bg-2); color: var(--muted); }
.row .ic .icon { width: 17px; height: 17px; }
.row .ic.ic-folder { color: var(--amber); }
.row .ic.ic-accent { color: var(--accent); }
.row .tx { flex: 1; min-width: 0; }
.row .t, .row .s { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.row .t { font-size: 15px; font-weight: 500; line-height: 1.3; }
.row .s { color: var(--muted); font-size: 13px; }
.row .n { display: -webkit-box; overflow: hidden; -webkit-line-clamp: 2; -webkit-box-orient: vertical; color: var(--muted); font-size: 13px; line-height: 1.4; }
.row .r { flex: none; color: var(--faint); font-size: 12px; }
.row.deleted .t { color: var(--muted); text-decoration: line-through; }
.pill { display: inline-flex; align-items: center; gap: 4px; padding: 2px 8px; border-radius: 999px; background: var(--bg-3); color: var(--muted); font-size: 12px; white-space: nowrap; }
.pill-new { background: var(--green-soft); color: var(--green); }
.pill-edited { background: var(--accent-soft); color: var(--accent); }
.pill-deleted { background: var(--red-soft); color: var(--red); }
.pill-type { background: var(--purple-soft); color: var(--purple); }
.avatar { width: 26px; height: 26px; flex: none; display: grid; place-items: center; border-radius: 50%; color: #ffffff; font-size: 11px; font-weight: 600; }
.avatar-0 { background: #9065b0; }
.avatar-1 { background: #448361; }
.avatar-2 { background: #cb912f; }
.avatar-3 { background: #2383e2; }
.avatar-4 { background: #d44c47; }
.avatar-5 { background: #2f8f8f; }
.avatar-6 { background: #c14c8a; }
.avatar-7 { background: #6b7280; }
.crumbs ol { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; margin: 10px 2px 2px; padding: 0; color: var(--muted); font-size: 13px; }
.crumbs li { margin: 0; list-style: none; }
.crumbs li + li::before { content: "\203A"; margin-right: 6px; color: var(--faint); }
.crumbs a { color: var(--muted); }
h1.page { margin: 6px 0; font-size: 28px; font-weight: 700; line-height: 1.2; letter-spacing: -.01em; }
.lede { margin: 0 0 12px; color: var(--muted); font-size: 15px; }
.hint { color: var(--muted); font-size: 14px; }
.notice { padding: 14px; border-radius: 12px; background: var(--bg-2); }
.notice p { margin: 0 0 8px; }
.notice p:last-child { margin-bottom: 0; }
.more-button {
    display: block; width: 100%; margin: 12px 0 0; padding: 10px; border: 0; border-radius: 8px;
    background: var(--bg-2); color: var(--accent); font-size: 14px; font-weight: 500;
}
.chips { display: flex; gap: 6px; margin: 6px 0; padding-bottom: 2px; overflow-x: auto; }
.chip {
    flex: none; width: auto; margin: 0; padding: 5px 11px; border: 0; border-radius: 999px;
    background: var(--bg-2); color: var(--muted); font-size: 13px;
}
.chip[aria-pressed="true"] { background: var(--text); color: var(--bg); }

/* ---------- Home ---------- */
.day { margin: 20px 4px 2px; color: var(--text); font-size: 13px; font-weight: 600; }
.group { margin: 12px 0 4px; }
.group-head { display: flex; align-items: center; gap: 10px; padding: 6px 4px; color: var(--text); font-size: 14px; }
.group-head .who { flex: 1; min-width: 0; }
.group-head .who b { font-weight: 600; }
.group-head .when { flex: none; color: var(--faint); font-size: 12px; }
.group-head.deleted .object { color: var(--muted); text-decoration: line-through; }
.group .items { margin-left: 17px; padding-left: 21px; border-left: 2px solid var(--line); }
.group-more { width: auto; margin: 0; padding: 6px; border: 0; background: none; color: var(--accent); font-size: 13px; }

/* ---------- Folder ---------- */
/* Rows, sections and the lede are the shared parts above. */

/* ---------- Concept ---------- */
.meta { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; margin: 8px 0 14px; color: var(--muted); font-size: 13px; }
.meta .fact + .fact::before { content: "\00B7"; margin-right: 6px; }
.concept-lede { margin: 0 0 16px; color: var(--muted); font: 19px/1.55 var(--serif); }
details.toc { margin: 6px 0 16px; border: 1px solid var(--line); border-radius: 10px; }
details.toc summary { display: flex; align-items: center; gap: 8px; padding: 10px 12px; color: var(--muted); font-size: 14px; font-weight: 500; list-style: none; }
details.toc summary::-webkit-details-marker { display: none; }
details.toc summary::after { display: none; }
details.toc[open] summary .icon-end { transform: rotate(180deg); }
details.toc ol { margin: 0; padding: 0 12px 10px 32px; font-size: 14px; }
details.toc li { padding: 4px 0; color: var(--muted); }
details.toc a { color: var(--muted); }
details.toc .depth-1 { margin-left: 14px; }
details.toc .depth-2 { margin-left: 28px; }
details.toc .depth-3 { margin-left: 42px; }
.markdown { overflow-wrap: anywhere; font-size: 16px; line-height: 1.65; }
.markdown.serif { font-family: var(--serif); font-size: 17.5px; line-height: 1.7; }
.markdown h1, .markdown h2, .markdown h3, .markdown h4, .markdown h5, .markdown h6 {
    margin: 26px 0 6px; color: var(--text); font-family: var(--sans); line-height: 1.3; letter-spacing: -.01em;
}
.markdown h1 { font-size: 22px; }
.markdown h2 { font-size: 20px; }
.markdown h3 { font-size: 17px; }
.markdown h4, .markdown h5, .markdown h6 { font-size: 15px; }
.markdown p, .markdown ul, .markdown ol, .markdown pre, .markdown table { margin: 0 0 14px; }
.markdown ul, .markdown ol { padding-left: 20px; }
.markdown li { margin: 3px 0; }
/* A small chip that does not change the line height. */
.markdown code { padding: 1px 5px; border-radius: 4px; background: var(--bg-3); color: var(--text); font: .8em/1 var(--mono); }
.markdown pre { padding: 12px; overflow-x: auto; border-radius: 8px; background: var(--bg-2); font-size: 13.5px; line-height: 1.5; }
.markdown pre code { padding: 0; background: none; font-size: inherit; line-height: inherit; }
.markdown a { color: var(--text); text-decoration: underline; text-decoration-color: var(--faint); text-underline-offset: 3px; }
.markdown blockquote { margin: 0 0 14px; padding: 10px 14px; border: 0; border-radius: 8px; background: var(--bg-2); color: var(--muted); font-size: .9em; }
.markdown blockquote > :last-child { margin-bottom: 0; }
.markdown table { display: block; width: auto; overflow-x: auto; font-family: var(--sans); font-size: 14px; }
.markdown img { max-width: 100%; height: auto; }
.markdown a[title="Not in this catalog"] { color: var(--red); text-decoration: line-through; }
.markdown [data-slug] { scroll-margin-top: calc(var(--touch) + 20px); }

/* ---------- Search ---------- */
#search-count { margin: 12px 2px 4px; font-size: 13px; }

/* ---------- Account and catalogs ---------- */
.card { display: flex; align-items: center; gap: 12px; padding: 14px; border-radius: 12px; background: var(--bg-2); }
.card .tx { min-width: 0; }
.card .s { display: block; color: var(--muted); font-size: 13px; }
.actions { display: flex; flex-direction: column; gap: 10px; margin-top: 20px; }
.actions button { width: 100%; margin: 0; }
```

- [ ] **Step 9: Run the tests**

Run: `tools/agentic/gate.sh narrow --lib -- http::read` and `tools/agentic/gate.sh narrow --lib -- http::ui`
Expected: PASS.

- [ ] **Step 10: Look at it once**

Open `docs/superpowers/specs/assets/2026-11-21-reader-polish-mockups.html` beside `/read/` on a local build at a phone width, in both schemes: the type, the neutrals and the tab bar match. (The screens are restyled by later tasks; only the base layer is checked here.)

- [ ] **Step 11: Gates, changelog and commit**

Run `fmt`, `lint`, `test`, `test-ui`, `dylint` through `gate.sh`. Expected: exit 0 (the journeys check behaviour, not styles).

`changelog/<ts>-reader-polish-design-layer.md`:

```markdown
# Reader: fonts, icons and design tokens

## Added
- Self-hosted Latin-subset woff2 fonts under the SIL Open Font License,
  with their license files: Inter (interface, weights 400 to 700) and
  Literata (concept bodies, roman and italic, 400 to 600), built by
  `scripts/subset-reader-fonts.sh` from the upstream release files
  (inputs: <the three SHA-256 lines the script printed>); outputs recorded
  in `scripts/reader-fonts.sha256`. About <N> KB in all, precached.
- `src/http/read/icons.svg`, the reader's icon sprite.
- `read.css` rewritten: Notion-like tokens for light, dark and an explicit
  `data-theme`, Pico's variables mapped onto them, and the component classes
  the screens use (rows, sections, pills, avatars, chips, timeline groups,
  the outline, the serif body).

## Fixed
- The system navigation bar was the old green accent: the manifest's colours
  are neutral now, and the page's `theme-color` metas give `#ffffff` in light
  and `#191919` in dark.

## Changed
- `.txt` files are served as `text/plain`.

## Tests
- The shell listing precaches the fonts, the sprite and the licenses; the
  theme-color metas and the manifest colours.
```

```bash
git add scripts src changelog
git commit -m "feat(read): self-hosted fonts, icon sprite and design tokens; neutral system bars

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 3: Pure display modules with Node tests

**Files:**
- Create: `src/http/read/js/text.js`, `timeline.js`, `outline.js`, `backlinks.js`, `avatar.js`
- Create: `tests/read_worker/text.test.mjs`, `timeline.test.mjs`, `outline.test.mjs`, `backlinks.test.mjs`, `avatar.test.mjs`

These modules hold no DOM and import nothing but each other (never a `/ui/...` path, which does not resolve in Node), so `tests/read_worker/page.mjs` loads them as they are. Day boundaries are the device's local days; the tests build their times with local-time `Date` constructors, so they pass in any time zone.

- [ ] **Step 1: Write the failing tests for `text.js` and `avatar.js`**

`tests/read_worker/text.test.mjs`:

```js
// Tests of the reader's display text helpers (src/http/read/js/text.js),
// run with `tools/agentic/gate.sh worker`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { fileName, folderTrail, docLabel, pages, readingMinutes, shortAgo, clock } = await page('text.js');

test('a path names its file and its folders', () => {
    assert.equal(fileName('/a/b/c.md'), 'c.md');
    assert.equal(fileName('c.md'), 'c.md');
    assert.equal(fileName(''), '');
    assert.equal(folderTrail('/infra/network/dns.md'), 'infra › network');
    assert.equal(folderTrail('/dns.md'), '');
    assert.equal(folderTrail(null), '');
});

test('a document is called by its title, else its file name', () => {
    assert.equal(docLabel({ path: '/a/b.md', title: 'Bee' }), 'Bee');
    assert.equal(docLabel({ path: '/a/b.md', title: null }), 'b.md');
    assert.equal(docLabel({ path: '/a/b.md', title: '' }), 'b.md');
});

test('page counts are grouped and singular for one', () => {
    assert.equal(pages(1284), '1,284 pages');
    assert.equal(pages(1), '1 page');
    assert.equal(pages(0), '0 pages');
});

test('reading time is at least a minute', () => {
    assert.equal(readingMinutes(0), 1);
    assert.equal(readingMinutes(1200), 1);
    assert.equal(readingMinutes(9000), 8);
    assert.equal(readingMinutes(undefined), 1);
});

test('a short age counts seconds, minutes, hours and days', () => {
    const now = Date.parse('2026-11-21T12:00:00Z');
    assert.equal(shortAgo('2026-11-21T11:59:33Z', now), '27 s');
    assert.equal(shortAgo('2026-11-21T11:55:00Z', now), '5 min');
    assert.equal(shortAgo('2026-11-21T09:00:00Z', now), '3 h');
    assert.equal(shortAgo('2026-11-19T12:00:00Z', now), '2 d');
    // A time a little ahead of the device's clock reads as now.
    assert.equal(shortAgo('2026-11-21T12:00:05Z', now), '0 s');
});

test('a clock time is hours and minutes on the 24-hour clock', () => {
    assert.equal(clock(new Date(2026, 10, 20, 16, 20).toISOString()), '16:20');
    assert.equal(clock(new Date(2026, 10, 20, 9, 2).toISOString()), '09:02');
});
```

`tests/read_worker/avatar.test.mjs`:

```js
// Tests of the avatar parts (src/http/read/js/avatar.js), run with
// `tools/agentic/gate.sh worker`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { AVATAR_COLOURS, initials, avatarClass } = await page('avatar.js');

test('initials take the first letters of the first two words', () => {
    assert.equal(initials('Alex Kim'), 'AK');
    assert.equal(initials('alex'), 'AL');
    assert.equal(initials('claude-code/model-name'), 'CC');
    assert.equal(initials('  sam  '), 'SA');
    assert.equal(initials('émile zola'), 'ÉZ');
    assert.equal(initials(''), '?');
    assert.equal(initials(null), '?');
});

test('the colour is stable for an actor and spread across actors', () => {
    assert.equal(avatarClass('alex'), avatarClass('alex'));
    const classes = new Set();
    for (let n = 0; n < 40; n += 1) {
        const name = avatarClass(`actor-${n}`);
        assert.match(name, /^avatar-\d$/);
        assert.ok(Number(name.slice('avatar-'.length)) < AVATAR_COLOURS, name);
        classes.add(name);
    }
    assert.ok(classes.size >= 4, `only ${classes.size} colours for 40 actors`);
    assert.match(avatarClass(''), /^avatar-\d$/);
});
```

- [ ] **Step 2: Run them to see them fail**

Run: `tools/agentic/gate.sh worker`
Expected: FAIL: `text.js` and `avatar.js` are not found (`ERR_MODULE_NOT_FOUND`).

- [ ] **Step 3: Write `text.js` and `avatar.js`**

`src/http/read/js/text.js`:

```js
// Display text the screens share: names from paths, counts, ages. No DOM
// and no imports, so Node tests it (tests/read_worker/text.test.mjs).

// The segments of a catalog path ('/a/b.md' -> ['a', 'b.md']).
function segments(path) {
    return String(path ?? '').split('/').filter(Boolean);
}

// The file name of a catalog path ('/a/b.md' -> 'b.md').
export function fileName(path) {
    const parts = segments(path);
    return parts[parts.length - 1] ?? '';
}

// The folders above `path`, for display ('/a/b/c.md' -> 'a > b' with a
// chevron); '' at the root.
export function folderTrail(path) {
    return segments(path).slice(0, -1).join(' › ');
}

// What a document ({ path, title }) is called: its title, else its file name.
export function docLabel(doc) {
    return doc?.title || fileName(doc?.path);
}

// '1,284 pages', '1 page'.
export function pages(count) {
    const n = Number(count) || 0;
    return `${n.toLocaleString('en-US')} ${n === 1 ? 'page' : 'pages'}`;
}

// Characters of prose read in a minute: about 230 words of 5 letters and a space.
export const CHARS_PER_MINUTE = 1200;

// Whole minutes to read `chars` characters, at least 1.
export function readingMinutes(chars) {
    return Math.max(1, Math.round((Number(chars) || 0) / CHARS_PER_MINUTE));
}

// How long ago `iso` was, short: '27 s', '5 min', '3 h', '2 d'. `now` is
// milliseconds since the epoch (Date.now() by default).
export function shortAgo(iso, now = Date.now()) {
    const seconds = Math.max(0, Math.round((now - Date.parse(iso)) / 1000));
    if (seconds < 60) return `${seconds} s`;
    if (seconds < 3600) return `${Math.floor(seconds / 60)} min`;
    if (seconds < 86400) return `${Math.floor(seconds / 3600)} h`;
    return `${Math.floor(seconds / 86400)} d`;
}

// The local time of `iso` on the 24-hour clock ('16:20').
export function clock(iso) {
    const date = new Date(iso);
    const pad = n => String(n).padStart(2, '0');
    return `${pad(date.getHours())}:${pad(date.getMinutes())}`;
}
```

`src/http/read/js/avatar.js`:

```js
// An actor's avatar: initials on a colour derived from a hash of the actor,
// so the same person keeps a colour on every screen and device. The colours
// are the classes avatar-0 to avatar-7 in read.css (the CSP allows no
// inline styles). No DOM, so Node tests it.

export const AVATAR_COLOURS = 8;

// The first letters of the first two words of `name` ('Alex Kim' -> 'AK'),
// or its first two letters when it is one word; '?' when it has none.
export function initials(name) {
    // \p{L}\p{N}: letters and digits in any script; everything else separates words.
    const words = String(name ?? '').split(/[^\p{L}\p{N}]+/u).filter(Boolean);
    if (!words.length) return '?';
    // [...word] splits by code point, so a letter outside the BMP stays whole.
    const letters = words.length === 1 ? [...words[0]].slice(0, 2) : [[...words[0]][0], [...words[1]][0]];
    return letters.join('').toUpperCase();
}

// The colour class of `actor` ('avatar-3'): FNV-1a over its UTF-16 code
// units, small and the same in every browser.
export function avatarClass(actor) {
    const text = String(actor ?? '');
    let hash = 0x811c9dc5;
    for (let at = 0; at < text.length; at += 1) {
        hash ^= text.charCodeAt(at);
        hash = Math.imul(hash, 0x01000193);
    }
    // >>> 0 reads the 32 bits as unsigned before the remainder.
    return `avatar-${(hash >>> 0) % AVATAR_COLOURS}`;
}
```

Run: `tools/agentic/gate.sh worker`
Expected: the `text` and `avatar` tests PASS.

- [ ] **Step 4: Write the failing timeline tests**

`tests/read_worker/timeline.test.mjs`:

```js
// Tests of Home's activity timeline (src/http/read/js/timeline.js), run with
// `tools/agentic/gate.sh worker`. Times are built with local-time Date
// constructors, so day boundaries hold in any time zone.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { BATCH_GAP_MS, FILTERS, GROUP_PREVIEW, changeKind, timeline, dayLabel, dayKey, verb, object, summary } = await page('timeline.js');

const NOW = new Date(2026, 10, 21, 12, 0, 0);
const at = (day, hour, minute, second = 0) => new Date(2026, 10, day, hour, minute, second).toISOString();

/** One change, newest-first order is the caller's. */
function change(path, { actor = 'alex', message = 'import', op = 'import', revision = 1, when = at(21, 11, 0), title = null } = {}) {
    return { path, title, description: null, revision, op, actor, actor_label: null, message, at: when };
}

test('a six-document import is one group of one day', () => {
    const changes = ['/f/a.md', '/f/b.md', '/f/c.md', '/f/d.md', '/f/e.md', '/f/g.md'].map(path => change(path));
    const days = timeline(changes, { now: NOW });
    assert.equal(days.length, 1);
    assert.equal(days[0].label, 'Today');
    assert.equal(days[0].groups.length, 1);
    assert.equal(days[0].groups[0].items.length, 6);
    assert.equal(summary(days[0].groups[0]), 'imported 6 pages');
    assert.equal(GROUP_PREVIEW, 3);
});

test('another actor or another message starts a new group', () => {
    const days = timeline([
        change('/a.md', { actor: 'alex', message: 'one' }),
        change('/b.md', { actor: 'sam', message: 'one' }),
        change('/c.md', { actor: 'sam', message: 'two' }),
        change('/d.md', { actor: 'sam', message: 'two' }),
    ], { now: NOW });
    assert.deepEqual(days[0].groups.map(group => group.items.length), [1, 1, 2]);
    assert.deepEqual(days[0].groups.map(group => group.actor), ['alex', 'sam', 'sam']);
});

test('a gap of two minutes or more starts a new group', () => {
    const close = timeline([
        change('/a.md', { when: at(21, 11, 2, 0) }),
        change('/b.md', { when: at(21, 11, 0, 1) }),
    ], { now: NOW });
    assert.equal(close[0].groups.length, 1, '1 min 59 s apart: one batch');
    const apart = timeline([
        change('/a.md', { when: at(21, 11, 2, 0) }),
        change('/b.md', { when: at(21, 11, 0, 0) }),
    ], { now: NOW });
    assert.equal(apart[0].groups.length, 2, '2 min apart: two batches');
    assert.equal(BATCH_GAP_MS, 120000);
});

test('a day boundary splits a batch, and days are labelled', () => {
    const days = timeline([
        change('/a.md', { when: at(21, 0, 0, 30) }),
        change('/b.md', { when: at(20, 23, 59, 30) }),
        change('/c.md', { when: at(18, 9, 0) }),
    ], { now: NOW });
    assert.equal(days.length, 3, 'a minute apart, but across midnight');
    assert.equal(days[0].label, 'Today');
    assert.equal(days[1].label, 'Yesterday');
    assert.match(days[2].label, /Nov 18/);
    assert.equal(days[0].key, dayKey(at(21, 0, 0, 30)));
    assert.equal(dayLabel(dayKey(NOW), NOW), 'Today');
});

test('each filter keeps its kind of change', () => {
    const changes = [
        change('/new.md', { op: 'create', revision: 1, actor: 'sam' }),
        change('/edited.md', { op: 'update', revision: 3 }),
        change('/imported.md', { op: 'import', revision: 2 }),
        change('/gone.md', { op: 'delete', revision: 4 }),
    ];
    const paths = (filter, me = null) => timeline(changes, { filter, me, now: NOW })
        .flatMap(day => day.groups.flatMap(group => group.items.map(item => item.path)));
    assert.deepEqual(paths('all'), ['/new.md', '/edited.md', '/imported.md', '/gone.md']);
    assert.deepEqual(paths('created'), ['/new.md']);
    assert.deepEqual(paths('edited'), ['/edited.md', '/imported.md']);
    assert.deepEqual(paths('deleted'), ['/gone.md']);
    assert.deepEqual(paths('mine', 'sam'), ['/new.md']);
    assert.deepEqual(paths('mine', null), [], 'signed out of the server: nothing is mine');
    assert.deepEqual(FILTERS.map(filter => filter.id), ['all', 'created', 'edited', 'deleted', 'mine']);
    assert.deepEqual(changes.map(changeKind), ['new', 'edited', 'edited', 'deleted']);
});

test('a group says what was done, to one document or to several', () => {
    const [single] = timeline([change('/k.md', { op: 'update', revision: 2, title: 'Kubernetes cluster' })], { now: NOW })[0].groups;
    assert.equal(verb(single), 'edited');
    assert.equal(object(single), 'Kubernetes cluster');
    assert.equal(summary(single), 'edited Kubernetes cluster');
    const [deleted] = timeline([change('/old.md', { op: 'delete', revision: 2 })], { now: NOW })[0].groups;
    assert.equal(summary(deleted), 'deleted old.md');
    const [mixed] = timeline([
        change('/a.md', { op: 'create', message: 'm' }),
        change('/b.md', { op: 'update', revision: 2, message: 'm' }),
    ], { now: NOW })[0].groups;
    assert.equal(summary(mixed), 'changed 2 pages');
});
```

Run: `tools/agentic/gate.sh worker`
Expected: FAIL: `timeline.js` is not found.

- [ ] **Step 5: Write `timeline.js`**

```js
// Home's activity timeline (reader polish design, "Home"): recent changes
// (newest first, as GET /catalogs/{c}/recent-changes lists them) grouped by
// the device's local day, then into batches: consecutive changes by the
// same actor with the same message, each less than BATCH_GAP_MS from the
// one before. Filters apply first. Presentational only, no DOM, so Node
// tests it (tests/read_worker/timeline.test.mjs).
import { docLabel, pages } from './text.js';

export const BATCH_GAP_MS = 2 * 60 * 1000;
// How many of a group's documents show before "+N more".
export const GROUP_PREVIEW = 3;

export const FILTERS = Object.freeze([
    { id: 'all', label: 'All' },
    { id: 'created', label: 'Created' },
    { id: 'edited', label: 'Edited' },
    { id: 'deleted', label: 'Deleted' },
    { id: 'mine', label: 'By me' },
]);

// What a change did to its document: 'new' (its first revision), 'edited'
// or 'deleted'. An import is new or edited by its revision too.
export function changeKind(change) {
    if (change.op === 'delete') return 'deleted';
    return change.revision === 1 ? 'new' : 'edited';
}

// Whether `change` shows under `filter` (a FILTERS id) for the user `me`
// (the session's user name, or null when unknown).
function matches(change, filter, me) {
    switch (filter) {
        case 'created': return changeKind(change) === 'new';
        case 'edited': return changeKind(change) === 'edited';
        case 'deleted': return changeKind(change) === 'deleted';
        case 'mine': return me !== null && change.actor === me;
        default: return true;
    }
}

// The local calendar day of `when` (an ISO time or a Date), 'YYYY-MM-DD'.
export function dayKey(when) {
    const date = new Date(when);
    const pad = n => String(n).padStart(2, '0');
    return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

// 'Today', 'Yesterday', else the date ('Wed, Nov 18').
export function dayLabel(key, now = new Date()) {
    if (key === dayKey(now)) return 'Today';
    const yesterday = new Date(now);
    yesterday.setDate(yesterday.getDate() - 1);
    if (key === dayKey(yesterday)) return 'Yesterday';
    const [year, month, day] = key.split('-').map(Number);
    return new Date(year, month - 1, day).toLocaleDateString('en-US', { weekday: 'short', month: 'short', day: 'numeric' });
}

// Whether `change` belongs to the batch `group` ends with.
function continues(group, change) {
    const last = group.items[group.items.length - 1];
    return last.actor === change.actor && last.message === change.message
        && Math.abs(Date.parse(last.at) - Date.parse(change.at)) < BATCH_GAP_MS;
}

// The timeline of `changes`: [{ key, label, groups: [{ actor, actorLabel,
// at, items }] }], days and groups newest first. `filter` is a FILTERS id,
// `me` the session's user name, `now` the device's time.
export function timeline(changes, { filter = 'all', me = null, now = new Date() } = {}) {
    const days = [];
    for (const change of changes ?? []) {
        if (!matches(change, filter, me)) continue;
        const key = dayKey(change.at);
        let day = days[days.length - 1];
        if (!day || day.key !== key) {
            day = { key, label: dayLabel(key, now), groups: [] };
            days.push(day);
        }
        const group = day.groups[day.groups.length - 1];
        if (group && continues(group, change)) group.items.push(change);
        else day.groups.push({ actor: change.actor, actorLabel: change.actor_label ?? null, at: change.at, items: [change] });
    }
    return days;
}

const VERBS = Object.freeze({ create: 'created', update: 'edited', delete: 'deleted', import: 'imported' });

// What a group did: 'imported', 'edited', ..., or 'changed' for mixed ops.
export function verb(group) {
    const ops = new Set(group.items.map(change => change.op));
    const [only] = ops;
    return ops.size === 1 ? (VERBS[only] ?? 'changed') : 'changed';
}

// What it did it to: the document's name for one, else 'N pages'.
export function object(group) {
    return group.items.length === 1 ? docLabel(group.items[0]) : pages(group.items.length);
}

// 'imported 6 pages', 'edited Kubernetes cluster'.
export function summary(group) {
    return `${verb(group)} ${object(group)}`;
}
```

Run: `tools/agentic/gate.sh worker`
Expected: the timeline tests PASS.

- [ ] **Step 6: Write the failing outline and backlink tests**

`tests/read_worker/outline.test.mjs`:

```js
// Tests of "On this page" (src/http/read/js/outline.js), run with
// `tools/agentic/gate.sh worker`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { sections, titleHeading } = await page('outline.js');

const heading = (text, level, slug = text.toLowerCase()) => ({ heading: text, slug, level, line: 1, lines: 1, chars: 1 });

test('a title heading and its sub-headings: only the title is dropped', () => {
    const outline = [heading('Beta', 1), heading('Settings', 2), heading('Deep', 3)];
    assert.equal(titleHeading(outline, 'Beta').slug, 'beta');
    assert.deepEqual(sections(outline, 'Beta').map(entry => [entry.heading, entry.depth]), [['Settings', 0], ['Deep', 1]]);
});

test('a concept made of level-1 sections lists every one of them', () => {
    const outline = [heading('One', 1), heading('Two', 1)];
    assert.equal(titleHeading(outline, 'Parts'), null);
    assert.deepEqual(sections(outline, 'Parts').map(entry => [entry.heading, entry.depth]), [['One', 0], ['Two', 0]]);
});

test('the title heading is recognised whatever its case, or when the concept has no title', () => {
    assert.equal(sections([heading('alpha ', 1), heading('One', 1)], 'Alpha').length, 1);
    assert.equal(sections([heading('Alpha', 1), heading('One', 1)], null).length, 1);
});

test('a first heading below level 1 is never the title', () => {
    const outline = [heading('Intro', 2), heading('More', 2)];
    assert.equal(titleHeading(outline, 'Intro'), null);
    assert.equal(sections(outline, 'Intro').length, 2);
    assert.deepEqual(sections([], 'X'), []);
    assert.deepEqual(sections(undefined, 'X'), []);
});
```

`tests/read_worker/backlinks.test.mjs`:

```js
// Tests of the "Linked from" rows (src/http/read/js/backlinks.js), run with
// `tools/agentic/gate.sh worker`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { dedupeBacklinks, backlinkNote } = await page('backlinks.js');

test('a document that links several times is one row with its count, in first-seen order', () => {
    const rows = dedupeBacklinks([
        { path: '/b.md', line: 3, title: 'B' },
        { path: '/a.md', line: 1, title: 'A' },
        { path: '/b.md', line: 7, title: 'B' },
        { path: '/b.md', line: 9, title: 'B' },
    ]);
    assert.deepEqual(rows, [{ path: '/b.md', title: 'B', count: 3 }, { path: '/a.md', title: 'A', count: 1 }]);
    assert.deepEqual(dedupeBacklinks(undefined), []);
});

test('the note counts links, and a log counts entries', () => {
    assert.equal(backlinkNote({ path: '/b.md', count: 3 }), '3 links');
    assert.equal(backlinkNote({ path: '/b.md', count: 1 }), '');
    assert.equal(backlinkNote({ path: '/infra/log.md', count: 6 }), '6 entries');
    assert.equal(backlinkNote({ path: '/log.md', count: 1 }), '1 entry');
});
```

Run: `tools/agentic/gate.sh worker`
Expected: FAIL: `outline.js` and `backlinks.js` are not found.

- [ ] **Step 7: Write `outline.js` and `backlinks.js`**

`src/http/read/js/outline.js`:

```js
// "On this page" (reader polish design, "Concept"): every heading of the
// concept's outline except the page title, whatever the levels. The title
// heading is the first heading when it is level 1 and reads as the title
// (or the concept has no title), so a concept written as "# Title" then
// "## Parts" lists its parts, and one made only of "#" sections lists them
// all. No DOM, so Node tests it.

// The outline entry that is the page title, or null.
export function titleHeading(headings, title) {
    const first = headings?.[0];
    if (!first || first.level !== 1) return null;
    const same = !title || first.heading.trim().toLowerCase() === String(title).trim().toLowerCase();
    return same ? first : null;
}

// The headings to list, each with `depth` (0 for the shallowest listed).
export function sections(headings, title) {
    const list = headings ?? [];
    const shown = titleHeading(list, title) ? list.slice(1) : list;
    const top = Math.min(...shown.map(heading => heading.level));
    return shown.map(heading => ({ ...heading, depth: heading.level - top }));
}
```

`src/http/read/js/backlinks.js`:

```js
// "Linked from" (reader polish design, "Concept"): GET /links lists one
// backlink per link, so a document that links three times appears three
// times. Here it is one row with a count. No DOM, so Node tests it.
import { fileName } from './text.js';

// [{ path, title, count }], one per linking document, in first-seen order.
export function dedupeBacklinks(backlinks) {
    const rows = new Map();
    for (const link of backlinks ?? []) {
        const row = rows.get(link.path);
        if (row) row.count += 1;
        else rows.set(link.path, { path: link.path, title: link.title ?? null, count: 1 });
    }
    return [...rows.values()];
}

// The row's note: '3 links' for a document that links more than once,
// '6 entries' for a log (each entry is one link), else ''.
export function backlinkNote(row) {
    if (fileName(row.path) === 'log.md') return `${row.count} ${row.count === 1 ? 'entry' : 'entries'}`;
    return row.count > 1 ? `${row.count} links` : '';
}
```

Run: `tools/agentic/gate.sh worker`
Expected: every reader test PASS, the new files included.

- [ ] **Step 8: Gates, changelog and commit**

Run `fmt`, `lint`, `test` (it runs `test-worker`), `dylint` through `gate.sh`. Expected: exit 0.

`changelog/<ts>-reader-polish-display-modules.md`:

```markdown
# Reader: display modules for the timeline, outline, backlinks and avatars

## Added
- `src/http/read/js/text.js` (names from paths, page counts, reading time,
  short ages), `timeline.js` (recent changes by day, then batches by actor
  and message less than 2 minutes apart, and the filter chips),
  `outline.js` (every heading but the page title), `backlinks.js` (one row
  per linking document with its count) and `avatar.js` (initials and a
  colour class from a hash of the actor). Pure modules, used by the screens
  in later tasks.

## Tests
- `tests/read_worker/{text,timeline,outline,backlinks,avatar}.test.mjs`.
```

```bash
git add src/http/read/js tests/read_worker changelog
git commit -m "feat(read): pure modules for the timeline, outline, backlinks and avatars

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 4: App chrome: header, tab icons, shared parts, scroll reset

**Files:**
- Create: `src/http/read/js/icons.js`
- Modify: `src/http/read/index.html` (body: header and tabs)
- Modify: `src/http/read/js/parts.js` (rewrite: shared parts)
- Modify: `src/http/read/js/main.js` (catalog list, header, scroll, `ctx`)
- Create: `tests/read_worker/tree-doubles/icons.mjs`; modify `tree-doubles/parts.mjs`, `tests/read_worker/esm-hooks.mjs`
- Modify: `src/http/read.rs` (test: every named icon is in the sprite)
- Modify: `tests/ui_e2e/read.rs` (helpers after `server_with_outline_limit`; two journeys after `signing_out_without_a_connection_still_deletes_the_offline_copies`)

After this task `parts.js`, `main.js`, the doubles and the journey helpers are frozen: Tasks 5 to 9 run in parallel on top of them.

- [ ] **Step 1: Write the failing journeys**

In `tests/ui_e2e/read.rs`, after `server_with_outline_limit`, add the helpers the later tasks share:

```rust
/// A server with one catalog `name` holding `entries`, sign-in routes on,
/// and the user `reader` entitled to read it.
///
/// # Arguments
/// * `name` - the catalog.
/// * `entries` - `(path, text)` of each file.
async fn server_with_catalog(name: &str, entries: &[(String, String)]) -> TestServer {
    let borrowed: Vec<(&str, &str)> = entries
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let server = TestServer::start_with(vec![(name, files(&borrowed))], |base| Config {
        public_url: Some(base.to_string()),
        ..Config::default()
    })
    .await;
    server.user_with_password("reader", EMAIL, PASSWORD).await;
    crate::support::server::applied(&server.app, async {
        ceiling::entitle(
            &server.db.pool,
            &NewEntitlement {
                user: "reader",
                catalog: name,
                access: Access::Read,
            },
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("entitle");
    })
    .await;
    server
}

/// A root index and `count` notes, `notes/n000.md` on, titled `N0` on,
/// each with `body` below its heading.
fn notes(count: usize, body: &str) -> Vec<(String, String)> {
    std::iter::once(("index.md".to_string(), "# Many\n".to_string()))
        .chain((0..count).map(|n| {
            (
                format!("notes/n{n:03}.md"),
                format!("---\ntitle: N{n}\n---\n# N{n}\n\n{body}\n"),
            )
        }))
        .collect()
}
```

After `signing_out_without_a_connection_still_deletes_the_offline_copies`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_header_shows_the_catalog_label_and_opens_the_picker() {
    let (server, _) = server().await;
    server.set_catalog_label("main", "Example Notes").await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/t/main", EMAIL, PASSWORD);
        ui.wait_text("#bar-label", "Example Notes");
        assert_eq!(ui.text("#bar-mark"), "E");
        assert_eq!(
            ui.eval("() => document.querySelector('#bar-search').getAttribute('href')", json!(null)),
            json!("#/s/main")
        );
        ui.click("#bar-catalog");
        ui.wait_visible("#catalog-list");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_screen_change_starts_at_the_top() {
    let server = server_with_catalog("many", &notes(60, "Body.")).await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/t/many/notes", EMAIL, PASSWORD);
        ui.wait_text("#concepts", "N59");
        ui.eval("() => { window.scrollTo(0, 1500); return true; }", json!(null));
        ui.poll_eval("() => window.scrollY > 0");
        ui.poll_eval("() => document.querySelector('#top').classList.contains('scrolled')");
        ui.click("#tab-home");
        ui.wait_visible("#screen-home");
        ui.poll_eval("() => window.scrollY === 0");
    })
    .await;
}
```

Run (with `TEST_PG_URL` set): `tools/agentic/gate.sh narrow --test ui_e2e -- read::the_header read::every_screen --test-threads=2`
Expected: FAIL: `#bar-label` does not exist; the scroll stays where it was.

- [ ] **Step 2: Write the failing sprite test**

In `src/http/read.rs` tests, at the end:

```rust
    /// Every icon the reader's page and scripts name by a literal
    /// (`icons.svg#i-NAME` in markup, `icon('NAME'` in a script) is a
    /// symbol of the sprite, so no icon draws empty.
    #[test]
    fn every_icon_the_reader_names_is_in_its_sprite() {
        let sprite = READ_ASSETS
            .iter()
            .find(|(name, _)| *name == "icons.svg")
            .map(|(_, body)| String::from_utf8_lossy(body).into_owned())
            .expect("icons.svg is embedded");
        let named = regex::Regex::new(r"icons\.svg#i-([a-z-]+)|icon\('([a-z-]+)'").expect("valid regex");
        let mut seen = 0;
        for (name, body) in READ_ASSETS {
            if !(name.ends_with(".js") || name.ends_with(".html")) {
                continue;
            }
            let text = String::from_utf8_lossy(body);
            for found in named.captures_iter(&text) {
                let icon = found
                    .get(1)
                    .or_else(|| found.get(2))
                    .map(|group| group.as_str())
                    .expect("one group matched");
                seen += 1;
                assert!(
                    sprite.contains(&format!("id=\"i-{icon}\"")),
                    "{name} names i-{icon}, which icons.svg lacks"
                );
            }
        }
        assert!(seen > 0, "the reader names no icon");
    }
```

Run: `tools/agentic/gate.sh narrow --lib -- http::read::tests::every_icon`
Expected: FAIL: "the reader names no icon".

- [ ] **Step 3: Write `icons.js`**

```js
// The reader's icons, from its sprite (/read/icons.svg, precached with the
// shell), as <svg class="icon"><use href="/read/icons.svg#i-NAME"></svg>.
// dom.js's h() makes HTML elements, and an <svg> drawn as HTML shows
// nothing, so icons are made here, in the SVG namespace.
const SVG = 'http://www.w3.org/2000/svg';
export const SPRITE = '/read/icons.svg';

// The icon `name` (a symbol id without its 'i-'), with extra classes `cls`.
export function icon(name, cls = '') {
    const svg = document.createElementNS(SVG, 'svg');
    svg.setAttribute('class', cls ? `icon ${cls}` : 'icon');
    svg.setAttribute('aria-hidden', 'true');
    svg.setAttribute('focusable', 'false');
    const use = document.createElementNS(SVG, 'use');
    use.setAttribute('href', `${SPRITE}#i-${name}`);
    svg.append(use);
    return svg;
}
```

- [ ] **Step 4: Header and tabs**

In `src/http/read/index.html`, replace the `<header class="bar">...</header>` and the `<nav class="tabs">...</nav>` blocks with:

```html
    <header class="top" id="top">
        <a href="#/c" id="bar-catalog" class="catalog">
            <span class="mark" id="bar-mark" aria-hidden="true">R</span>
            <span class="label" id="bar-label">Reader</span>
            <svg class="icon icon-sm" aria-hidden="true" focusable="false"><use href="/read/icons.svg#i-down"/></svg>
        </a>
        <span class="spacer"></span>
        <a href="#/c" id="bar-search" class="iconbtn" aria-label="Search">
            <svg class="icon" aria-hidden="true" focusable="false"><use href="/read/icons.svg#i-search"/></svg>
        </a>
    </header>
```

```html
    <nav class="tabs" aria-label="Reader">
        <a id="tab-home" href="#/"><svg class="icon" aria-hidden="true" focusable="false"><use href="/read/icons.svg#i-home"/></svg><span>Home</span></a>
        <a id="tab-browse" href="#/c"><svg class="icon" aria-hidden="true" focusable="false"><use href="/read/icons.svg#i-folder"/></svg><span>Browse</span></a>
        <a id="tab-search" href="#/c"><svg class="icon" aria-hidden="true" focusable="false"><use href="/read/icons.svg#i-search"/></svg><span>Search</span></a>
        <a id="tab-me" href="#/me"><svg class="icon" aria-hidden="true" focusable="false"><use href="/read/icons.svg#i-user"/></svg><span>Account</span></a>
    </nav>
```

- [ ] **Step 5: Rewrite `parts.js`**

```js
// View parts more than one screen uses. Frozen once the screens are built
// on them: a screen that needs a new part keeps it in its own file.
import { h } from '/ui/js/core/dom.js';
import { fromBrowse, hrefs } from './route.js';
import { icon } from './icons.js';

// The search box, which opens the search screen for `catalog`.
export function searchForm(catalog, query = '', placeholder = 'Search') {
    const input = h('input', { type: 'search', name: 'q', id: 'search-input', placeholder, value: query, 'aria-label': 'Search', enterkeyhint: 'search' });
    return h('form', {
        id: 'search-form', role: 'search',
        onsubmit: event => {
            event.preventDefault();
            const text = input.value.trim();
            if (text) location.hash = hrefs.search(catalog, text);
        },
    }, icon('search'), input);
}

// Breadcrumbs from the catalog root to the folder holding `path` (or to
// `path` itself when it is a folder). `root` names the catalog (its label;
// its name by default); `titles` (a Map of folder path to title, from the
// folders' indexes) names the folders that have a title.
export function crumbs(catalog, path, isFolder, { root = catalog, titles = null } = {}) {
    const parts = String(path ?? '').split('/').filter(Boolean);
    const folders = isFolder ? parts : parts.slice(0, -1);
    const items = [h('li', {}, h('a', { href: hrefs.tree(catalog) }, root))];
    folders.forEach((name, at) => {
        const folder = folders.slice(0, at + 1).join('/');
        items.push(h('li', {}, h('a', { href: hrefs.tree(catalog, folder) }, titles?.get(folder) ?? name)));
    });
    return h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb' }, h('ol', {}, items));
}

// body_html from src/render.rs, made the reader's: links to the console's
// browse routes go to reader routes, web links open outside the app.
// `serif` sets the concept reading type.
export function renderBody(html, { serif = false } = {}) {
    const article = h('article', { class: serif ? 'markdown serif' : 'markdown', id: 'body' });
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

// A list row: an icon tile (`glyph`, tinted by `tint`), a title, an optional
// line under it (`sub`), an optional note of up to two lines (`note`), and
// what goes at its end (`end`: a count, a pill, an icon). A link when
// `href` is given.
export function row({ href = null, glyph = 'doc', tint = null, title, sub = null, note = null, end = null, id = null, cls = null }) {
    return h(href ? 'a' : 'div', { class: cls ? `row ${cls}` : 'row', href, id },
        h('span', { class: tint ? `ic ic-${tint}` : 'ic' }, icon(glyph)),
        h('span', { class: 'tx' },
            h('span', { class: 't' }, title),
            sub ? h('span', { class: 's' }, sub) : null,
            note ? h('span', { class: 'n' }, note) : null),
        end);
}

// A section heading ('Pages · 42' with `count`).
export function sectionHead(title, { count = null, id = null } = {}) {
    return h('div', { class: 'sec', id }, h('h2', {}, count === null ? title : `${title} · ${count}`));
}

// A pill; `kind` is new, edited, deleted or type (read.css .pill-<kind>).
export function pill(text, kind = null) {
    return h('span', { class: kind ? `pill pill-${kind}` : 'pill' }, text);
}

// A screen's title.
export function pageTitle(text, id = null) {
    return h('h1', { class: 'page', id }, text);
}

// A lede paragraph, or null without text (h() and fill() drop it).
export function lede(text, cls = 'lede') {
    return text ? h('p', { class: cls }, text) : null;
}
```

The existing screens keep working: `searchForm(catalog, query)`, `crumbs(catalog, path, isFolder)` and `renderBody(html)` keep their meaning with the new parameters left out.

- [ ] **Step 6: Doubles for the screens' Node tests**

`tests/read_worker/tree-doubles/icons.mjs`:

```js
// A fake of src/http/read/js/icons.js: an icon as a plain object naming it.
export function icon(name, cls = '') {
    return { tag: 'svg', attrs: { class: cls ? `icon ${cls}` : 'icon', 'data-icon': name }, children: [] };
}
```

Replace `tests/read_worker/tree-doubles/parts.mjs` with:

```js
// The shared view parts as plain objects (the real ones need a DOM), with
// the same arguments as src/http/read/js/parts.js.
const kids = (...children) => children.flat(Infinity).filter(child => child !== null && child !== undefined && child !== false);

export function crumbs(catalog, folder, isFolder = false, names = {}) {
    return { tag: 'nav', attrs: { class: 'crumbs' }, children: [], catalog, folder, isFolder, root: names.root ?? catalog, titles: names.titles ?? null };
}

export function renderBody(html, options = {}) {
    return { tag: 'article', attrs: { id: 'body' }, children: [], html, serif: options.serif ?? false };
}

export function message(text, action = null, id = 'notice') {
    return { tag: 'div', attrs: { id }, children: kids(text, action) };
}

export function row({ href = null, glyph = 'doc', tint = null, title, sub = null, note = null, end = null, id = null, cls = null }) {
    return { tag: href ? 'a' : 'div', attrs: { class: cls ? `row ${cls}` : 'row', href, id }, glyph, tint, children: kids(title, sub, note, end) };
}

export function sectionHead(title, { count = null, id = null } = {}) {
    return { tag: 'div', attrs: { class: 'sec', id }, children: [count === null ? title : `${title} · ${count}`] };
}

export function pill(text, kind = null) {
    return { tag: 'span', attrs: { class: kind ? `pill pill-${kind}` : 'pill' }, children: [text] };
}

export function pageTitle(text, id = null) {
    return { tag: 'h1', attrs: { class: 'page', id }, children: [text] };
}

export function lede(text, cls = 'lede') {
    return text ? { tag: 'p', attrs: { class: cls }, children: [text] } : null;
}
```

In `tests/read_worker/esm-hooks.mjs`, add `['../icons.js', 'icons.mjs'],` to `DOUBLED`, and say so in its header comment ("... and icons.js").

- [ ] **Step 7: `main.js`: catalog list, header, scroll, `ctx`**

Change the imports:

```js
import { read, readAll, signOut, ReadError, KIND } from './api.js';
```

Replace `const state = { session: null, catalog: remembered() };` with:

```js
// `known`: the catalogs the account reaches, by name (GET /catalogs: label,
// page count), once read; `titles`: each catalog's folder titles (folder
// path to title), as its tree was last read, for breadcrumbs.
const state = { session: null, catalog: remembered(), known: null, titles: new Map() };
// The catalog list's read, shared by every screen of this page load.
let catalogRead = null;
```

Add after `freshness`:

```js
// The catalogs the account reaches, by name, read once per page load (the
// worker keeps a copy for offline). A refused read gives an empty map and
// is tried again next time: the header then shows the catalog's name, and
// each screen's own reads decide what the refusal means.
function catalogList() {
    catalogRead ??= readAll('/catalogs', 'catalogs').then(result => {
        state.known = new Map(result.data.map(found => [found.name, found]));
        header();
        return state.known;
    }, error => {
        catalogRead = null;
        if (error instanceof ReadError) return new Map();
        throw error;
    });
    return catalogRead;
}

// The header: the current catalog's mark and label, and where its search
// icon goes.
function header() {
    const catalog = state.catalog;
    const label = (catalog && state.known?.get(catalog)?.label) || catalog || 'Reader';
    $('#bar-label').textContent = label;
    $('#bar-mark').textContent = ([...label.trim()][0] ?? 'R').toUpperCase();
    $('#bar-search').setAttribute('href', catalog ? hrefs.search(catalog) : hrefs.catalogs());
}
```

In `tabs()`, replace the line `$('#bar-catalog').textContent = catalog ?? 'Reader';` with `header();`.

In `show()`, after `clear($('#reader')).append(view);` add:

```js
    // Every screen change starts at the top (a section route scrolls to its
    // heading once drawn).
    window.scrollTo(0, 0);
```

and extend `ctx` with:

```js
        // The current catalog's entry of GET /catalogs ({ label, concepts, ... }), or null.
        summary: () => catalogList().then(found => found.get(route.catalog) ?? null),
        // Its label now: its name until the list has been read.
        label: () => state.known?.get(route.catalog)?.label ?? route.catalog,
        // Its folder titles from the last tree read, or null.
        titles: () => state.titles.get(route.catalog) ?? null,
        rememberTitles: titles => { state.titles.set(route.catalog, titles); },
```

Add after `const WORKER_WAIT_MS = 5000;`:

```js
// The reader decides where each screen starts (at the top), not the browser.
if ('scrollRestoration' in history) history.scrollRestoration = 'manual';
// The header's hairline shows once the page is scrolled.
window.addEventListener('scroll', () => {
    $('#top').classList.toggle('scrolled', window.scrollY > 4);
}, { passive: true });
```

In `boot()`, after the session `try`/`catch` and before `$('#reader').hidden = false;`, start the catalog list's read (not awaited: the first screen does not wait for it):

```js
    catalogList();
```

- [ ] **Step 8: Run the tests**

Run: `tools/agentic/gate.sh narrow --lib -- http::read::tests::every_icon` (PASS), `tools/agentic/gate.sh worker` (PASS: `tree.test.mjs` and `catalogs.test.mjs` still pass with the new doubles), then `tools/agentic/gate.sh narrow --test ui_e2e -- read:: --test-threads=2`
Expected: PASS, the two new journeys included.

- [ ] **Step 9: Gates, changelog and commit**

Run `fmt`, `lint`, `test`, `test-ui`, `dylint`. Expected: exit 0.

`changelog/<ts>-reader-polish-chrome.md`:

```markdown
# Reader: header, tab icons and scroll position

## Added
- The header shows the current catalog's label (not its name) after a mark
  with its first letter, opens the picker, and has a search icon; it gets a
  hairline once the page scrolls. The bottom tabs have icons.
- `parts.js` gains the shared parts the restyled screens use (`row`,
  `sectionHead`, `pill`, `pageTitle`, `lede`; breadcrumbs with the catalog
  label and folder titles; the serif body), and `icons.js` draws icons from
  the sprite.
- `main.js` reads the catalog list once per page load, for labels and page
  counts.

## Fixed
- The scroll position carried over between screens: every screen change now
  starts at the top.

## Tests
- Journeys: the header shows the catalog label and opens the picker; a tab
  change after scrolling starts at the top. A unit test checks that every
  icon the reader names is in the sprite.
```

```bash
git add src tests changelog
git commit -m "feat(read): header with the catalog label, tab icons, shared parts, scroll reset

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 5: Home: search with page count, Jump back in, activity timeline

**Files:**
- Rewrite: `src/http/read/js/screens/home.js`
- Modify: `tests/ui_e2e/read.rs`: `home_lists_recent_changes_and_search_finds_a_concept`, `home_lists_cached_recent_changes_and_viewed_concepts_offline`, `a_late_refusal_from_home_evicts_the_catalog_it_asked_about` (the held suffix), `HOME_HINT` and the dismissal eval in `the_install_hint_shows_on_an_iphone_browser_until_dismissed` (the fake `ctx`); new journeys after `home_lists_cached_recent_changes_and_viewed_concepts_offline`

**Depends on:** Tasks 1 (titles in recent changes), 3 (`timeline.js`, `avatar.js`, `text.js`), 4 (`parts.js`, `ctx.summary`).

`#recent` is now the timeline's container. Each group's documents are all in the DOM (those past the first three `hidden` until "+N more"), so a journey that reads `#recent`'s text or follows one of its links still finds every document of the page.

- [ ] **Step 1: Update the journeys that read Home, and write the new ones**

In `home_lists_recent_changes_and_search_finds_a_concept` and in `home_lists_cached_recent_changes_and_viewed_concepts_offline`, `ui.wait_text("#recent", "systems/alpha.md")` becomes:

```rust
        ui.wait_text("#recent", "Alpha");
```

(the timeline shows the title, with the folder under it). In `a_late_refusal_from_home_evicts_the_catalog_it_asked_about`, the held read becomes `ui.hold_next("GET", "/catalogs/main/recent-changes?limit=50");`. In `HOME_HINT` and in the dismissal eval of `the_install_hint_shows_on_an_iphone_browser_until_dismissed`, the fake context of `home.render` becomes:

```js
{ route: { catalog: 'main' }, state: { session: null }, live: () => true, freshness: () => {}, summary: async () => null }
```

After `home_lists_cached_recent_changes_and_viewed_concepts_offline`, add:

```rust
/// The group of `#recent` whose heading says `who`, as `[shown, all]`
/// counts of its documents, or `null` when there is none.
const GROUP_ROWS: &str = "(who) => { \
    const group = [...document.querySelectorAll('#recent .group')] \
        .find(g => g.querySelector('.who').textContent.includes(who)); \
    if (!group) return null; \
    const rows = [...group.querySelectorAll('.items .row')]; \
    return [rows.filter(r => !r.hidden).length, rows.length]; }";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_import_shows_as_one_group_that_expands() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        signed_in_at_home(ui);
        ui.wait_text("#recent", "imported 6 pages");
        // One group for the whole import, three documents shown.
        assert_eq!(ui.eval(GROUP_ROWS, json!("imported 6 pages")), json!([3, 6]));
        assert_eq!(
            ui.eval(
                "() => [...document.querySelectorAll('#recent .who')].filter(w => w.textContent.includes('imported')).length",
                json!(null)
            ),
            json!(1)
        );
        ui.wait_text("#recent .group-more", "+3 more");
        ui.click("#recent .group-more");
        assert_eq!(ui.eval(GROUP_ROWS, json!("imported 6 pages")), json!([6, 6]));
        assert!(!ui.present("#recent .group-more"));
        // A chip filters: the fixture deletes nothing.
        ui.click("[data-filter=deleted]");
        ui.wait_visible("#recent-empty");
        ui.click("[data-filter=all]");
        ui.wait_text("#recent", "imported 6 pages");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn show_older_reads_the_next_page_of_changes() {
    // 60 notes and a root index: more changes than one page of 50.
    let server = server_with_catalog("many", &notes(60, "Body.")).await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/t/many", EMAIL, PASSWORD);
        ui.open_reader("/");
        ui.wait_visible("#recent-older");
        let rows = "() => document.querySelectorAll('#recent .row, #recent a.group-head').length";
        let first = ui.eval(rows, json!(null));
        ui.click("#recent-older");
        ui.wait_hidden("#recent-older");
        let all = ui.eval(rows, json!(null));
        assert!(all.as_u64() > first.as_u64(), "{first} then {all}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn jump_back_in_lists_the_concepts_read_last() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        signed_in_at_home(ui);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        ui.open_reader("/");
        ui.wait_text("#jump-back", "Alpha");
        ui.wait_text("#jump-back", "read ");
        // Online, the offline list is not drawn: Jump back in stands for it.
        assert!(!ui.present("#offline-list"));
        // A search box that counts the catalog's pages.
        ui.poll_eval(
            "() => document.querySelector('#search-input').placeholder.startsWith('Search ') \
             && document.querySelector('#search-input').placeholder.endsWith('pages')",
        );
    })
    .await;
}
```

Run: `tools/agentic/gate.sh narrow --test ui_e2e -- read::home read::an_import read::show_older read::jump_back --test-threads=2`
Expected: FAIL (no groups, no `#recent-older`, no `#jump-back`; the fake contexts already pass).

- [ ] **Step 2: Rewrite `home.js`**

```js
// Home: the search box (with the catalog's page count), "Jump back in" (the
// concepts read last, from the offline copies), and the catalog's activity:
// recent changes by day and batch (timeline.js), with filter chips and
// "Show older". When the server cannot be reached, or the worker answered
// from its copy, every offline copy is listed instead, under "Available
// offline". On an iPhone's browser (not yet installed) it says how to install.
import { h, fill } from '/ui/js/core/dom.js';
import { ago } from '/ui/js/core/format.js';
import { read, enc, KIND, ReadError } from '../api.js';
import { offlineDocuments } from '../cache.js';
import { hrefFor } from '../route.js';
import { searchForm, message, row, sectionHead, pill } from '../parts.js';
import { icon } from '../icons.js';
import { FILTERS, GROUP_PREVIEW, changeKind, object, timeline, verb } from '../timeline.js';
import { avatarClass, initials } from '../avatar.js';
import { clock, docLabel, folderTrail, pages, shortAgo } from '../text.js';

const HINT_KEY = 'okf.read.install-hint';
// Changes per page: the server's default.
const PAGE = 50;
// Rows of "Jump back in".
const JUMP_BACK = 3;

// The install hint, or null when it does not apply (not an iPhone or iPad
// browser, already installed, or dismissed before).
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

// One offline copy ({ path, title, cachedAt }) as a row.
function copyRow(catalog, copy) {
    const where = [folderTrail(copy.path), `read ${ago(copy.cachedAt)}`].filter(Boolean).join(' · ');
    return row({ href: hrefFor(catalog, copy.path), glyph: 'doc', title: docLabel(copy), sub: where, end: icon('offline', 'chev') });
}

// "Jump back in" (online: the newest JUMP_BACK copies, nothing when there
// are none), or "Available offline" (`all`: every copy, or a hint).
function drawCopies(view, catalog, copies, all) {
    if (!all) {
        if (!copies.length) return;
        view.append(sectionHead('Jump back in'),
            h('div', { class: 'rows', id: 'jump-back' }, copies.slice(0, JUMP_BACK).map(copy => copyRow(catalog, copy))));
        return;
    }
    view.append(sectionHead('Available offline'));
    if (!copies.length) {
        view.append(h('p', { class: 'hint', id: 'offline-list' }, 'Nothing yet. Concepts you open are kept for reading offline.'));
        return;
    }
    view.append(h('div', { class: 'rows', id: 'offline-list' }, copies.map(copy => copyRow(catalog, copy))));
}

// One document of a group: its title (struck through and not a link when
// deleted), its folder, and a pill for what happened to it.
function itemRow(catalog, change) {
    const kind = changeKind(change);
    return row({
        href: kind === 'deleted' ? null : hrefFor(catalog, change.path),
        glyph: 'doc', title: docLabel(change), sub: folderTrail(change.path) || null,
        end: pill(kind, kind), cls: kind === 'deleted' ? 'deleted' : null,
    });
}

// One group: "Name imported 6 pages" with its documents (the first
// GROUP_PREVIEW shown, then "+N more"), or, for one change, a single row
// that opens the document (not for a deletion).
function groupView(catalog, group, today) {
    const first = group.items[0];
    const single = group.items.length === 1;
    const deleted = single && changeKind(first) === 'deleted';
    const linked = single && !deleted;
    const who = group.actorLabel ?? group.actor;
    const head = h(linked ? 'a' : 'div', {
        class: deleted ? 'group-head deleted' : 'group-head',
        href: linked ? hrefFor(catalog, first.path) : null,
    },
    h('span', { class: `avatar ${avatarClass(group.actor)}`, 'aria-hidden': 'true' }, initials(who)),
    h('span', { class: 'who' }, h('b', {}, who), ` ${verb(group)} `, h('span', { class: 'object' }, object(group))),
    h('span', { class: 'when' }, today ? shortAgo(group.at) : clock(group.at)));
    if (single) return h('div', { class: 'group' }, head);
    const items = group.items.map(change => itemRow(catalog, change));
    items.slice(GROUP_PREVIEW).forEach(item => { item.hidden = true; });
    const rest = items.length - GROUP_PREVIEW;
    const more = rest > 0 ? h('button', { type: 'button', class: 'group-more' }, `+${rest} more`) : null;
    more?.addEventListener('click', () => {
        for (const item of items) item.hidden = false;
        more.remove();
    });
    return h('div', { class: 'group' }, head, h('div', { class: 'items' }, items, more));
}

// The activity section: chips, the timeline of every page read so far, and
// "Show older" while the server has more.
function drawActivity(view, ctx, catalog, first) {
    const me = ctx.state.session?.user ?? null;
    const base = `/catalogs/${enc(catalog)}/recent-changes?limit=${PAGE}`;
    const loaded = { changes: [...first.data.changes], next: first.data.next_after ?? null, filter: 'all' };
    const list = h('div', { class: 'timeline', id: 'recent' });
    const older = h('button', { type: 'button', class: 'more-button', id: 'recent-older' }, 'Show older');
    const chips = FILTERS.map(filter => h('button', {
        type: 'button', class: 'chip', dataset: { filter: filter.id },
        onclick: () => { loaded.filter = filter.id; redraw(); },
    }, filter.label));
    function redraw() {
        for (const chip of chips) chip.setAttribute('aria-pressed', String(chip.dataset.filter === loaded.filter));
        older.hidden = loaded.next === null;
        const days = timeline(loaded.changes, { filter: loaded.filter, me, now: new Date() });
        if (!days.length) {
            fill(list, h('p', { class: 'hint', id: 'recent-empty' }, loaded.changes.length ? 'No changes match.' : 'No changes yet.'));
            return;
        }
        fill(list, days.map(day => [
            h('h3', { class: 'day' }, day.label),
            day.groups.map(group => groupView(catalog, group, day.label === 'Today')),
        ]));
    }
    older.addEventListener('click', async () => {
        older.disabled = true;
        try {
            const page = await read(`${base}&after=${enc(loaded.next)}`);
            // The user may have left meanwhile: touch neither the banner nor a newer screen.
            if (!ctx.live()) return;
            ctx.freshness(page);
            loaded.changes.push(...page.data.changes);
            const next = page.data.next_after ?? null;
            // A cursor that does not move would never end: stop at it.
            loaded.next = next === loaded.next ? null : next;
            redraw();
        } catch (error) {
            if (!(error instanceof ReadError)) throw error;
            if (error.kind !== KIND.UNAVAILABLE) {
                // A 401 or a lost grant here wipes or evicts like any other read.
                await ctx.fail(error);
                return;
            }
            if (ctx.live() && !document.getElementById('older-offline')) {
                older.after(message('Older changes need a connection.', null, 'older-offline'));
            }
        } finally {
            older.disabled = false;
        }
    });
    view.append(sectionHead('Activity'),
        h('div', { class: 'chips', role: 'group', 'aria-label': 'Show changes' }, chips),
        list, older);
    redraw();
}

export async function render(view, ctx) {
    const catalog = ctx.route.catalog;
    const box = searchForm(catalog);
    // fill, not append: the hint is null except on an iPhone's browser.
    fill(view, installHint(), box);
    // The page count comes with the catalog list (main.js); the box works without it.
    ctx.summary().then(known => {
        if (known && ctx.live()) box.querySelector('input').placeholder = `Search ${pages(known.concepts)}`;
    });
    let result;
    try {
        result = await read(`/catalogs/${enc(catalog)}/recent-changes?limit=${PAGE}`);
    } catch (error) {
        if (error.kind !== KIND.UNAVAILABLE) throw error;
        const copies = await offlineDocuments(catalog);
        if (ctx.live()) drawCopies(view, catalog, copies, true);
        return;
    }
    const copies = await offlineDocuments(catalog);
    // After an await the user may have left: draw nothing, touch no banner.
    if (!ctx.live()) return;
    ctx.freshness(result);
    drawCopies(view, catalog, copies, result.offline);
    drawActivity(view, ctx, catalog, result);
}
```

- [ ] **Step 3: Run the journeys**

Run: `tools/agentic/gate.sh narrow --test ui_e2e -- read:: --test-threads=2`
Expected: PASS, the three new journeys and the updated ones included. (`a_log_in_recent_changes_opens_as_a_document` still finds `#recent a[href='#/d/main/log.md']`: the log is a document of the import group and of the catalog's creation group.)

- [ ] **Step 4: Compare with the mockup**

At a phone width, in both schemes, Home against the mockup's Activity direction (Views, Direction 3, Home): chips, day headings, avatar groups, pills, "+N more". Fix spacing only inside read.css's `Home` section.

- [ ] **Step 5: Gates, changelog and commit**

Run `fmt`, `lint`, `test`, `test-ui`, `dylint`. Expected: exit 0.

`changelog/<ts>-reader-polish-home.md`:

```markdown
# Reader: Home with Jump back in and an activity timeline

## Changed
- Home's search box counts the catalog's pages ("Search 1,284 pages").
- "Jump back in" lists the three concepts read last (from the offline
  copies), each with its folder, when it was read and an offline mark.
- "Recent changes" became an activity timeline: filter chips (All, Created,
  Edited, Deleted, By me), day headings, and groups by actor and batch (the
  same actor and message, less than 2 minutes apart) with an avatar,
  "Name imported 6 pages", the time, and up to three documents with a pill,
  then "+N more". Deletions are struck through and not links. "Show older"
  reads the next page. Titles replace paths.
- Offline, or from the worker's copy, every offline copy is listed under
  "Available offline", as before.
- Home reads 50 changes a page (was 20).

## Tests
- Journeys: an import is one group that expands; a chip filters; Show
  older reads the next page; Jump back in lists the concept read last; the
  search box counts pages. Updated: the journeys that read `#recent`.
```

```bash
git add src tests changelog
git commit -m "feat(read): Home with Jump back in and an activity timeline

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 6: Folder: About row, folders with descriptions and counts, concept rows, Log row

**Files:**
- Modify: `src/http/read/js/listing.js` (rewrite)
- Rewrite: `src/http/read/js/screens/tree.js`
- Modify: `src/http/read/js/screens/document.js`
- Modify: `tests/read_worker/route.test.mjs` (the `folderListing` tests), rewrite `tests/read_worker/tree.test.mjs`
- Modify: `tests/ui_e2e/read.rs`: `the_picker_and_the_tree_lead_from_a_catalog_to_its_concepts`, the `#document-title` lines of `a_log_opens_as_a_document` and `a_log_in_recent_changes_opens_as_a_document`, `a_folder_whose_index_is_deleted_meanwhile_still_lists_its_concepts` (replaced), `a_slow_success_cannot_put_back_a_deleted_folder_index`

**Depends on:** Tasks 1 (`description` in the tree), 4 (parts, `ctx.label`, `ctx.rememberTitles`).

The folder screen reads only the tree now (spec change 5): the index's title and description head the page, and its body opens behind "About" as a document (`#/d/<catalog>/<folder>/index.md`, which the document screen and the refusal handler already serve and evict). A deleted index is therefore handled by the document screen's refusal (`KIND.CONCEPT_GONE` evicts `documents/<path>`), and the tree screen has no index read to test.

- [ ] **Step 1: Write the failing listing tests**

In `tests/read_worker/route.test.mjs`, import `folderTitles` too (`const { folderListing, folderTitles } = await page('listing.js');`), give the `TREE` entry `/systems/index.md` a description (`{ path: '/systems/index.md', kind: 'index', title: 'Systems', description: 'What runs where.' }`), and replace the three `folderListing` tests with:

```js
test('folderListing lists the root\'s folders, index, log and concepts by label', () => {
    const listing = folderListing(TREE, '');
    assert.deepEqual(listing.folders.map(entry => entry.name), ['people', 'systems', 'systemsx']);
    assert.equal(listing.index.path, '/index.md');
    assert.equal(listing.log.path, '/log.md');
    assert.deepEqual(listing.concepts.map(doc => doc.path), ['/alpha.md', '/zeta.md']);
    // Without a title, a concept is labelled by its file name.
    assert.equal(listing.label(listing.concepts[0]), 'alpha.md');
    assert.equal(listing.label(listing.concepts[1]), 'Zeta');
});

test('folderListing gives each subfolder its index title and description and its page count', () => {
    const systems = folderListing(TREE, '').folders.find(entry => entry.name === 'systems');
    // beta.md and deep/gamma.md: nested concepts count too.
    assert.deepEqual(systems, { name: 'systems', path: 'systems', title: 'Systems', description: 'What runs where.', pages: 2 });
    const [deep] = folderListing(TREE, 'systems').folders;
    assert.deepEqual(deep, { name: 'deep', path: 'systems/deep', title: null, description: null, pages: 1 });
});

test('folderListing lists one folder and none of its namesakes', () => {
    const listing = folderListing(TREE, 'systems');
    assert.deepEqual(listing.folders.map(entry => entry.name), ['deep']);
    assert.equal(listing.index.path, '/systems/index.md');
    assert.equal(listing.log, null);
    assert.deepEqual(listing.concepts.map(doc => doc.path), ['/systems/beta.md']);
});

test('folderListing of a folder without an index or documents is empty', () => {
    assert.deepEqual(folderListing(TREE, 'systems/deep').index, null);
    const empty = folderListing(TREE, 'nowhere');
    assert.deepEqual([empty.folders, empty.index, empty.log, empty.concepts], [[], null, null, []]);
});

test('folderTitles names every folder whose index has a title', () => {
    assert.deepEqual([...folderTitles(TREE)], [['systems', 'Systems']]);
});
```

Run: `tools/agentic/gate.sh worker`
Expected: FAIL: `folders` holds strings; `log` and `folderTitles` do not exist.

- [ ] **Step 2: Rewrite `listing.js`**

```js
// One folder of a catalog's tree (GET /catalogs/{c}/tree's documents:
// [{ path: '/a/b.md', kind, title, description }]): its index, its log, its
// subfolders (each with its own index's title and description, and how many
// concepts it holds, nested ones included) and its concepts. Machinery
// files are left out of the reader.

// A document's name in a list: its title, else its file name.
export function label(doc) {
    return doc.title ?? doc.path.split('/').pop();
}

export function folderListing(documents, folder) {
    const prefix = folder ? `/${folder}/` : '/';
    const folders = new Map();
    const concepts = [];
    let index = null;
    let log = null;
    const subfolder = name => {
        if (!folders.has(name)) {
            folders.set(name, { name, path: folder ? `${folder}/${name}` : name, title: null, description: null, pages: 0 });
        }
        return folders.get(name);
    };
    for (const doc of documents) {
        if (!doc.path.startsWith(prefix)) continue;
        const rest = doc.path.slice(prefix.length);
        const slash = rest.indexOf('/');
        if (slash >= 0) {
            const entry = subfolder(rest.slice(0, slash));
            if (doc.kind === 'concept') entry.pages += 1;
            // The subfolder's own index (not a deeper one) names it.
            else if (doc.kind === 'index' && rest.slice(slash + 1) === 'index.md') {
                entry.title = doc.title ?? null;
                entry.description = doc.description ?? null;
            }
        } else if (doc.kind === 'index') index = doc;
        else if (doc.kind === 'log') log = doc;
        else if (doc.kind === 'concept') concepts.push(doc);
    }
    concepts.sort((a, b) => label(a).localeCompare(label(b)));
    const sorted = [...folders.values()].sort((a, b) => (a.title ?? a.name).localeCompare(b.title ?? b.name));
    return { folders: sorted, index, log, concepts, label };
}

// Every folder's title by its path ('a/b'), from the folders' indexes, for
// breadcrumbs. The root's index names the catalog's page, not a folder.
export function folderTitles(documents) {
    const titles = new Map();
    for (const doc of documents) {
        if (doc.kind !== 'index' || !doc.title) continue;
        const folder = doc.path.replace(/^\/+/, '').split('/').slice(0, -1).join('/');
        if (folder) titles.set(folder, doc.title);
    }
    return titles;
}
```

Run: `tools/agentic/gate.sh worker`
Expected: the `folderListing` and `folderTitles` tests PASS. `tree.test.mjs` now FAILS (the old screen reads `folders` as names); the next steps replace both.

- [ ] **Step 3: Write the failing folder screen tests**

Replace `tests/read_worker/tree.test.mjs` with:

```js
// Tests of the folder screen (src/http/read/js/screens/tree.js) run in Node
// (`tools/agentic/gate.sh worker`): esm-hooks.mjs maps its imports to
// tree-doubles/, a fake DOM and a tree read the test answers, so what the
// screen draws from the tree, a refusal, and an answer that arrives after
// the user has left can each be checked. The screen reads only the tree:
// the folder's index opens behind "About", as a document.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { KIND, ReadError } = await page('api.js');
const { render } = await page('screens/tree.js', '?doubles');

/** A promise with its `resolve`, settled by the test. */
function deferred() {
    let resolve;
    const promise = new Promise(yes => { resolve = yes; });
    return { promise, resolve };
}

const ticks = () => new Promise(resolve => setTimeout(resolve, 0));

const TREE = {
    documents: [
        { path: '/index.md', kind: 'index', title: 'Main', description: 'Everything we keep.' },
        { path: '/log.md', kind: 'log', title: null, description: null },
        { path: '/overview.md', kind: 'concept', title: 'Overview', description: 'Start here.' },
        { path: '/systems/index.md', kind: 'index', title: 'Systems', description: 'What runs where.' },
        { path: '/systems/alpha.md', kind: 'concept', title: 'Alpha', description: null },
        { path: '/systems/deep/beta.md', kind: 'concept', title: 'Beta', description: null },
    ],
};

/** The elements of `nodes` (and theirs) matching `test`. */
function find(nodes, test) {
    const found = [];
    const walk = node => {
        if (!node || typeof node !== 'object') return;
        if (test(node)) found.push(node);
        node.children?.forEach(walk);
    };
    nodes.forEach(walk);
    return found;
}

const byId = (nodes, id) => find(nodes, node => node.attrs?.id === id);
const text = node => (typeof node === 'string' ? node : (node.children ?? []).map(text).join(' '));

/**
 * One render of `folder`: `answer` gives the tree read's answer (TREE at
 * once by default). Returns the world: the reads, what was appended, the
 * freshness reports, the titles remembered, and the `live` switch.
 */
function scene({ folder = '', answer = async () => ({ data: TREE, offline: false }) } = {}) {
    const world = { reads: [], appended: [], fresh: [], titles: [], live: true };
    globalThis.treeWorld = {
        read: async path => { world.reads.push(path); return answer(path); },
    };
    world.view = { append: (...nodes) => { world.appended.push(...nodes); } };
    world.ctx = {
        route: { catalog: 'main', folder },
        live: () => world.live,
        freshness: result => { world.fresh.push(result); },
        label: () => 'Main catalog',
        rememberTitles: titles => { world.titles.push(titles); },
    };
    world.run = () => render(world.view, world.ctx);
    return world;
}

test('the root reads only the tree and draws About, its lede, folders, concepts and its log', async () => {
    const world = scene();
    await world.run();
    assert.deepEqual(world.reads, ['/catalogs/main/tree']);
    assert.equal(text(byId(world.appended, 'folder-title')[0]), 'Main');
    assert.equal(find(world.appended, node => node.attrs?.class === 'lede').map(text).join(), 'Everything we keep.');
    assert.equal(byId(world.appended, 'about')[0].attrs.href, '#/d/main/index.md');
    const folders = text(byId(world.appended, 'folders')[0]);
    assert.match(folders, /Systems/);
    assert.match(folders, /What runs where\./);
    assert.match(folders, /\b2\b/, 'alpha and deep/beta');
    const concepts = byId(world.appended, 'concepts')[0];
    assert.match(text(concepts), /Overview/);
    assert.match(text(concepts), /Start here\./);
    assert.doesNotMatch(text(concepts), /log/i);
    assert.match(text(find(world.appended, node => node.attrs?.class === 'sec' && text(node).startsWith('Pages'))[0]), /Pages · 1/);
    assert.equal(byId(world.appended, 'folder-log')[0].attrs.href, '#/d/main/log.md');
    assert.equal(world.fresh.length, 1);
    assert.equal(world.titles[0].get('systems'), 'Systems');
    const [crumbs] = find(world.appended, node => node.tag === 'nav');
    assert.equal(crumbs.root, 'Main catalog');
});

test('a folder without an index has no About row and is titled by its name', async () => {
    const world = scene({ folder: 'systems/deep' });
    await world.run();
    assert.equal(byId(world.appended, 'about').length, 0);
    assert.equal(text(byId(world.appended, 'folder-title')[0]), 'deep');
    assert.equal(find(world.appended, node => node.attrs?.class === 'lede').length, 0);
    assert.match(text(byId(world.appended, 'concepts')[0]), /Beta/);
    assert.equal(byId(world.appended, 'folders').length, 0);
    assert.equal(byId(world.appended, 'folder-log').length, 0);
});

test('a refused tree read is thrown to the screen handler and draws nothing', async () => {
    for (const kind of [KIND.FAILED, KIND.LOCKED, KIND.CATALOG_GONE, KIND.SIGNED_OUT, KIND.UNAVAILABLE]) {
        const error = new ReadError(kind, 'refused');
        const world = scene({ answer: async () => { throw error; } });
        await assert.rejects(world.run(), caught => caught === error, kind);
        assert.deepEqual(world.appended, [], kind);
    }
});

test('a tree that answers after the user has left draws nothing and reports nothing', async () => {
    const answer = deferred();
    const world = scene({ answer: () => answer.promise });
    const rendering = world.run();
    await ticks();
    world.live = false;
    answer.resolve({ data: TREE, offline: false });
    await rendering;
    assert.deepEqual(world.appended, []);
    assert.deepEqual(world.fresh, []);
    assert.deepEqual(world.titles, []);
});
```

Run: `tools/agentic/gate.sh worker`
Expected: FAIL: no `folder-title`, `about`, `folder-log`.

- [ ] **Step 4: Rewrite `tree.js`**

```js
// One folder of the tree: its breadcrumbs, its title (its index's title, or
// its name) and its index's description, an About row for the index's text,
// its subfolders (title, description, page count) and its concepts (title
// and description), then its log. Only the tree is read: About opens the
// index as a document.
import { h } from '/ui/js/core/dom.js';
import { read, enc } from '../api.js';
import { hrefs } from '../route.js';
import { folderListing, folderTitles } from '../listing.js';
import { icon } from '../icons.js';
import { crumbs, lede, pageTitle, row, sectionHead } from '../parts.js';

export async function render(view, ctx) {
    const { catalog, folder } = ctx.route;
    const result = await read(`/catalogs/${enc(catalog)}/tree`);
    if (!ctx.live()) return;
    ctx.freshness(result);
    const documents = result.data.documents;
    const titles = folderTitles(documents);
    ctx.rememberTitles(titles);
    const listing = folderListing(documents, folder);
    const name = folder ? folder.split('/').pop() : ctx.label();
    // Conditional parts are appended one by one: the native append would
    // print a null.
    view.append(crumbs(catalog, folder, true, { root: ctx.label(), titles }),
        pageTitle(listing.index?.title ?? name, 'folder-title'));
    if (listing.index?.description) view.append(lede(listing.index.description));
    if (listing.index) {
        view.append(h('div', { class: 'rows' }, row({
            id: 'about', href: hrefs.document(catalog, listing.index.path),
            glyph: 'info', tint: 'accent', title: 'About', sub: 'What this folder is for',
        })));
    }
    if (listing.folders.length) {
        view.append(sectionHead('Folders'), h('div', { class: 'rows', id: 'folders' }, listing.folders.map(entry => row({
            href: hrefs.tree(catalog, entry.path), glyph: 'folder', tint: 'folder',
            title: entry.title ?? entry.name, sub: entry.description,
            end: [h('span', { class: 'r' }, String(entry.pages)), icon('chev', 'chev')],
        }))));
    }
    if (listing.concepts.length) {
        view.append(sectionHead('Pages', { count: listing.concepts.length }),
            h('div', { class: 'rows', id: 'concepts' }, listing.concepts.map(doc => row({
                href: hrefs.concept(catalog, doc.path), glyph: 'doc', title: listing.label(doc), sub: doc.description,
            }))));
    }
    if (listing.log) {
        view.append(h('div', { class: 'rows' }, row({
            id: 'folder-log', href: hrefs.document(catalog, listing.log.path),
            glyph: 'log', title: 'Log', sub: 'What changed in this folder',
        })));
    }
}
```

Run: `tools/agentic/gate.sh worker`
Expected: PASS.

- [ ] **Step 5: Name About and Log pages**

Replace `src/http/read/js/screens/document.js` with:

```js
// A folder's index.md (its About page), a log.md or AGENTS.md: breadcrumbs,
// its name and its rendered text.
import { read, enc, encPath } from '../api.js';
import { crumbs, pageTitle, renderBody } from '../parts.js';

const NAMES = Object.freeze({ 'index.md': 'About', 'log.md': 'Log' });

export async function render(view, ctx) {
    const { catalog, path } = ctx.route;
    const result = await read(`/catalogs/${enc(catalog)}/documents/${encPath(path)}`);
    if (!ctx.live()) return;
    ctx.freshness(result);
    const file = path.split('/').pop();
    view.append(crumbs(catalog, path, false, { root: ctx.label(), titles: ctx.titles() }),
        pageTitle(NAMES[file] ?? file, 'document-title'),
        renderBody(result.data.body_html));
}
```

- [ ] **Step 6: Update the journeys**

Add `use okf_mcp::labels::UNTITLED_CATALOG;` to the imports of `tests/ui_e2e/read.rs`. In `a_log_opens_as_a_document` and `a_log_in_recent_changes_opens_as_a_document`, `ui.wait_text("#document-title", "log.md");` becomes `ui.wait_text("#document-title", "Log");`.

Replace the body of `the_picker_and_the_tree_lead_from_a_catalog_to_its_concepts` after `ui.click("#tab-browse");` with:

```rust
        // The root folder: an About row for its index (whose text is no
        // longer drawn above the lists), its folders, and its log on a row
        // of its own, not among the concepts (the root has none).
        ui.wait_text("#folders", "systems");
        ui.wait_visible("#about");
        assert!(!ui.present("#body"), "the index's text is behind About");
        assert_eq!(ui.text("#concepts"), "", "logs are not listed as concepts");
        ui.wait_visible("#folder-log");
        // The catalog list has been read: breadcrumbs name the catalog by its label.
        ui.wait_text("#bar-label", UNTITLED_CATALOG);
        ui.click("#folders a");
        // systems/: its two concepts by title, under the catalog's label.
        ui.wait_text("#concepts", "Alpha");
        ui.wait_text("#concepts", "Beta");
        ui.wait_text("#folder-title", "systems");
        ui.wait_text(".crumbs", UNTITLED_CATALOG);
        // About opens the index's text.
        ui.click("#about");
        ui.wait_text("#document-title", "About");
        ui.wait_text("#body", "Systems");
```

Replace `a_folder_whose_index_is_deleted_meanwhile_still_lists_its_concepts` with:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deleted_folder_index_drops_its_about_copy_and_the_folder_still_lists() {
    let (server, _) = server().await;
    let runtime = tokio::runtime::Handle::current();
    let (pool, app) = (server.db.pool.clone(), std::sync::Arc::clone(&server.app));
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/d/main/systems/index.md");
        ui.wait_text("#body", "Systems");
        let paths = content_paths(ui);
        assert!(
            paths
                .iter()
                .any(|path| path.ends_with("/documents/systems/index.md")),
            "{paths:?}"
        );
        runtime.block_on(publish(
            &pool,
            &app,
            "main",
            main_files_without("systems/index.md"),
        ));
        // About again: the index is gone, so its copy goes and the screen says so.
        ui.open_reader("/me");
        ui.wait_visible("#account-user");
        ui.open_reader("/d/main/systems/index.md");
        ui.wait_visible("#concept-gone");
        let paths = content_paths(ui);
        assert!(
            paths
                .iter()
                .all(|path| !path.ends_with("/documents/systems/index.md")),
            "{paths:?}"
        );
        // Its folder still lists its concepts, now without an About row.
        ui.click("#gone-folder");
        ui.wait_text("#concepts", "Alpha");
        assert!(!ui.present("#about"));
    })
    .await;
}
```

In `a_slow_success_cannot_put_back_a_deleted_folder_index`, `ui.open_reader("/t/main/systems");` becomes `ui.open_reader("/d/main/systems/index.md");`, and the comment before the direct eviction becomes: "A fresh read of the About page would get the 404 and evict (journey `a_deleted_folder_index_drops_its_about_copy...`): make that eviction directly, while the old 200 is still held."

Run: `tools/agentic/gate.sh narrow --test ui_e2e -- read:: --test-threads=2`
Expected: PASS.

- [ ] **Step 7: Compare with the mockup, then gates, changelog and commit**

At a phone width, in both schemes, against the mockup's Pages direction, Folder screen. Then run `fmt`, `lint`, `test`, `test-ui`, `dylint` (exit 0).

`changelog/<ts>-reader-polish-folder.md`:

```markdown
# Reader: the folder screen

## Changed
- A folder shows breadcrumbs (the catalog's label, then folder titles), its
  title (its index's title, or its name) and its index's description as a
  lede, then an About row for the index's text, which is no longer drawn
  above the lists.
- Subfolders show an icon, their title, a one-line description, their page
  count (nested concepts included) and a chevron; concepts their title and
  a one-line description under "Pages · N"; the folder's log is a final
  "Log" row. The folder screen reads only the tree.
- A folder's index and log open as "About" and "Log".

## Tests
- `route.test.mjs`: the listing's titles, descriptions, counts and log.
  `tree.test.mjs`: what the folder draws from the tree, a refused tree, a
  late answer. Journeys: the picker and tree journey checks About and the
  Log row; a deleted folder index evicts its About copy (replacing the
  journey that deleted it under the folder screen).
```

```bash
git add src tests changelog
git commit -m "feat(read): folder screen with About, described folders and concepts, and a Log row

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 7: Concept: meta row, lede, On this page, serif body, whole body, deduplicated backlinks

**Files:**
- Rewrite: `src/http/read/js/screens/concept.js`
- Modify: `tests/ui_e2e/read.rs`: the five `SHORT_OUTLINE` journeys, `offline_a_concept_opens_from_either_its_short_or_its_full_copy`, the concept-read suffixes in `an_unlock_refusal_keeps_the_caches_and_points_at_the_console`, `a_lost_grant_answered_after_navigating_away_still_evicts`, `a_late_401_after_an_ordinary_refusal_still_signs_out`, `a_slow_success_cannot_put_back_an_evicted_catalog`, `a_slow_success_cannot_put_back_a_deleted_concept`; new journeys after `a_concept_shows_its_body_follows_links_and_lists_backlinks`

**Depends on:** Tasks 3 (`outline.js`, `backlinks.js`, `text.js`), 4 (parts, `ctx.label`, `ctx.titles`).

The screen always reads `?full=true`: `concept_outline_chars` exists so MCP agents do not pull a large body into context, and the reader shows the whole body anyway. The "Show the whole concept" button, its notices (`#shortened`, `#show-full`, `#full-failed`) and their journeys go. The worker's two-way copy matching (`sw.js` `cachedCopy`) stays: a plain copy cached before this milestone still answers the `?full=true` read offline.

- [ ] **Step 1: Write the failing journeys**

After `a_concept_shows_its_body_follows_links_and_lists_backlinks`:

```rust
/// A catalog whose concept `sections.md` is made of level-1 sections only,
/// with a description and tags, linked three times by `links.md`.
fn sectioned() -> Vec<(String, String)> {
    [
        ("index.md", "# Deep\n"),
        (
            "sections.md",
            "---\ntype: system\ntitle: Sections\ndescription: Two parts, no title heading.\n\
             tags: [infra, k8s]\n---\n# One\n\nFirst `code` part.\n\n# Two\n\nSecond part.\n",
        ),
        (
            "links.md",
            "---\ntitle: Links\n---\n# Links\n\nSee [it](sections.md).\n\nAgain [one](sections.md#one).\n\n\
             And [two](sections.md#two).\n",
        ),
    ]
    .iter()
    .map(|(path, text)| (path.to_string(), text.to_string()))
    .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concept_of_level_one_sections_has_an_outline_and_a_meta_row() {
    let server = server_with_catalog("deep", &sectioned()).await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/v/deep/sections.md", EMAIL, PASSWORD);
        ui.wait_text("#concept-title", "Sections");
        // Both level-1 sections are listed (only a title heading is dropped).
        ui.wait_text("#outline summary", "2 sections");
        assert!(ui.text("#outline").contains("One") && ui.text("#outline").contains("Two"));
        // The meta row: type, tags, reading time, the last edit.
        ui.wait_text("#concept-meta", "system");
        ui.wait_text("#concept-meta", "k8s");
        ui.wait_text("#concept-meta", "min read");
        ui.wait_text("#concept-meta", "Edited");
        ui.wait_text(".concept-lede", "Two parts, no title heading.");
        // The body is set in the serif reading type.
        assert_eq!(
            ui.eval("() => document.querySelector('#body').classList.contains('serif')", json!(null)),
            json!(true)
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_document_that_links_three_times_shows_once_with_a_count() {
    let server = server_with_catalog("deep", &sectioned()).await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/v/deep/sections.md", EMAIL, PASSWORD);
        ui.wait_text("#backlinks", "Links");
        assert_eq!(
            ui.eval(
                "() => [...document.querySelectorAll('#backlinks .row')].filter(r => r.textContent.includes('Links')).length",
                json!(null)
            ),
            json!(1)
        );
        ui.wait_text("#backlinks", "3 links");
    })
    .await;
}
```

Replace `the_whole_concept_replaces_the_outline_notice`, `the_whole_concept_is_not_available_offline_and_can_be_retried` and `show_the_whole_concept_after_losing_the_grant_evicts_the_catalog` with:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_concept_renders_whole_without_a_button() {
    let (server, _) = server_with_outline_limit(SHORT_OUTLINE).await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/v/main/systems/beta.md", EMAIL, PASSWORD);
        ui.wait_text("#body", "Two settings");
        assert!(!ui.present("#show-full") && !ui.present("#shortened"));
        // Its title heading is the page title, so the body does not repeat it.
        assert!(!ui.present("#body [data-slug='beta']"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_concept_read_once_is_readable_whole_offline() {
    let (server, _) = server_with_outline_limit(SHORT_OUTLINE).await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_text("#body", "Two settings");
        let offline = ui.go_offline();
        ui.reload_reader();
        ui.wait_text("#body", "Two settings");
        ui.wait_text("#offline-banner", "Offline. Synced");
        drop(offline);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_concept_after_losing_the_grant_evicts_the_catalog() {
    let (server, _) = server_with_outline_limit(SHORT_OUTLINE).await;
    let runtime = tokio::runtime::Handle::current();
    let (pool, app) = (server.db.pool.clone(), std::sync::Arc::clone(&server.app));
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        lose_main(&runtime, &pool, &app);
        ui.open_reader("/v/main/systems/beta.md");
        // The refusal evicts the catalog and goes to the picker, naming it.
        ui.wait_visible("#screen-catalogs");
        assert_eq!(ui.eval("() => location.hash", json!(null)), json!("#/c?gone=main"));
        ui.wait_visible("#catalog-gone");
        ui.poll_eval(NO_MAIN_CONTENT);
    })
    .await;
}
```

(`lose_main` and `NO_MAIN_CONTENT` are defined further down the file; Rust items may be used before they are written.)

In `an_empty_concept_has_no_show_whole_button`, keep the body; it still holds. Replace `a_late_whole_concept_does_not_touch_a_newer_screen` with the same journey for the concept's own read (the `fetch` override is unchanged: the screen's read ends in `?full=true`):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_concept_answer_does_not_touch_a_newer_screen() {
    let (server, _) = server_with_outline_limit(SHORT_OUTLINE).await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.wait_visible("#account-user");
        // The concept read answers late, as the worker's cached copy would
        // (the offline header): held in the page until released.
        ui.eval(
            "() => { const real = window.fetch; window.lateRelease = null; \
             window.fetch = (input, init) => { \
               if (!String(input).endsWith('?full=true')) return real(input, init); \
               return new Promise(resolve => { window.lateRelease = async () => { \
                 const answer = await real(input, init); \
                 resolve(new Response(await answer.text(), { status: 200, headers: { \
                   'Content-Type': 'application/json', 'X-Read-Offline': '1', \
                   'X-Read-Cached-At': new Date().toISOString() } })); }; }); }; }",
            json!(null),
        );
        ui.open_reader("/v/main/systems/beta.md");
        ui.poll_eval("() => typeof window.lateRelease === 'function'");
        // Leave for another screen, then let the answer through.
        ui.open_reader("/me");
        ui.wait_visible("#account-user");
        ui.eval("() => window.lateRelease()", json!(null));
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(
            ui.eval(
                "() => document.querySelector('#offline-banner').hidden",
                json!(null)
            ),
            json!(true),
            "the late answer marked the new screen offline"
        );
        assert!(ui.present("#account-user") && !ui.present("#body"));
    })
    .await;
}
```

In `an_outline_link_in_a_long_concept_keeps_the_whole_body`, delete the line `ui.click("#show-full");` (the body is whole at once; the rest holds).

Replace `offline_a_concept_opens_from_either_its_short_or_its_full_copy` with:

```rust
/// Reads beta plainly (no `?full`) from the page, through the worker, as a
/// reader before the polish milestone did; returns the status.
const READ_BETA_PLAIN: &str = "async () => \
    (await fetch('/api/v1/catalogs/main/concepts/systems/beta.md')).status";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_a_concept_opens_from_either_its_plain_or_its_full_copy() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        // A plain copy of beta, as an older reader kept it.
        assert_eq!(ui.eval(READ_BETA_PLAIN, json!(null)), json!(200));
        // Alpha opened in the reader: cached with ?full=true.
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        let offline = ui.go_offline();
        // The screen asks for ?full=true: beta's plain copy answers it.
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_text("#body", "Two settings");
        // A plain read of alpha: its full copy answers.
        assert_eq!(
            ui.eval(
                "async () => { const answer = await fetch('/api/v1/catalogs/main/concepts/systems/alpha.md'); \
                 return [answer.status, answer.headers.has('X-Read-Offline')]; }",
                json!(null)
            ),
            json!([200, true])
        );
        drop(offline);
    })
    .await;
}
```

The concept read's URL now ends in `?full=true`, so the journeys that hold or fail it by suffix change:
- `an_unlock_refusal_keeps_the_caches_and_points_at_the_console`: `fail_next_kind("GET", "/concepts/systems/beta.md?full=true", ...)`.
- `a_lost_grant_answered_after_navigating_away_still_evicts`: `ui.hold_next("GET", "/concepts/systems/beta.md?full=true");`.
- `a_late_401_after_an_ordinary_refusal_still_signs_out`: `wait_answered(ui, "/concepts/systems/missing.md?full=true");`.
- `a_slow_success_cannot_put_back_an_evicted_catalog`: `ui.pause_worker_responses("*/concepts/systems/beta.md*");` and `ui.hold_answer_later("GET", "/concepts/systems/beta.md?full=true");`.
- `a_slow_success_cannot_put_back_a_deleted_concept`: `ui.hold_answer_later("GET", "/concepts/systems/alpha.md?full=true");` (its pattern already ends in `*`).

Delete the `SHORT_OUTLINE` doc comment's second line ("with a button for the whole concept") and say instead "the reader still shows it whole".

Run: `tools/agentic/gate.sh narrow --test ui_e2e -- read:: --test-threads=2`
Expected: the new journeys FAIL (no `#outline` for level-1 sections, no `#concept-meta`, three backlink rows), and `a_long_concept_renders_whole_without_a_button` FAILS (`#shortened` shows).

- [ ] **Step 2: Rewrite `concept.js`**

```js
// A concept: breadcrumbs, its title, a meta row (type, up to three tags, the
// last edit, reading time), its description as a lede, "On this page", the
// body in the serif reading type, and what links to it, one row per
// document. The reader always reads the whole body (?full=true): the
// server's outline limit is meant for agents.
import { h, fill } from '/ui/js/core/dom.js';
import { ago } from '/ui/js/core/format.js';
import { read, enc, encPath, KIND, ReadError } from '../api.js';
import { hrefs, hrefFor, parentOf } from '../route.js';
import { crumbs, lede, pageTitle, pill, renderBody, row, sectionHead } from '../parts.js';
import { icon } from '../icons.js';
import { sections, titleHeading } from '../outline.js';
import { backlinkNote, dedupeBacklinks } from '../backlinks.js';
import { fileName, readingMinutes } from '../text.js';

const MAX_TAGS = 3;

// The links read, or null when offline with no copy: the concept still shows.
async function backlinks(base, path) {
    try {
        return await read(`${base}/links/${encPath(path)}`);
    } catch (error) {
        if (error.kind === KIND.UNAVAILABLE) return null;
        throw error;
    }
}

// The newest revision ({ actor, actor_label, at }), or null: the meta row
// does without it offline (the worker keeps no history) or when the history
// cannot be read. A refusal that matters refuses the concept's read too,
// which acts on it.
async function lastEdit(base, path) {
    try {
        const result = await read(`${base}/history/${encPath(path)}?limit=1`);
        return result.data.revisions?.[0] ?? null;
    } catch (error) {
        if (error instanceof ReadError) return null;
        throw error;
    }
}

// The meta row: the type and tags as pills, then the last edit and the
// reading time.
function metaRow(concept, edit) {
    const fields = concept.frontmatter ?? {};
    const tags = Array.isArray(fields.tags) ? fields.tags.filter(tag => typeof tag === 'string').slice(0, MAX_TAGS) : [];
    return h('div', { class: 'meta', id: 'concept-meta' },
        typeof fields.type === 'string' && fields.type ? pill(fields.type, 'type') : null,
        tags.map(tag => pill(tag)),
        edit ? h('span', { class: 'fact' }, `Edited ${ago(edit.at)} by ${edit.actor_label ?? edit.actor}`) : null,
        h('span', { class: 'fact' }, `${readingMinutes(concept.chars)} min read`));
}

// "On this page": every heading but the page title, linking to its section.
function onThisPage(catalog, path, listed) {
    if (!listed.length) return null;
    return h('details', { class: 'toc', id: 'outline' },
        h('summary', {}, icon('list'), `On this page · ${listed.length} ${listed.length === 1 ? 'section' : 'sections'}`, icon('down', 'icon-end')),
        h('ol', {}, listed.map(heading => h('li', { class: `depth-${Math.min(heading.depth, 3)}` },
            h('a', { href: hrefs.concept(catalog, path, heading.slug) }, heading.heading)))));
}

// "Linked from": one row per linking document, with its count.
function linkedFrom(catalog, links, rootLabel) {
    if (!links) return [sectionHead('Linked from'), h('p', { class: 'hint', id: 'backlinks' }, 'Links are not available offline.')];
    const rows = dedupeBacklinks(links.data.backlinks);
    if (!rows.length) return [sectionHead('Linked from'), h('p', { class: 'hint', id: 'backlinks' }, 'Nothing links here.')];
    return [sectionHead('Linked from', { count: rows.length }), h('div', { class: 'rows', id: 'backlinks' }, rows.map(link => {
        const name = fileName(link.path);
        const folder = parentOf(link.path).split('/').pop() || rootLabel;
        const title = name === 'log.md' ? 'Log' : link.title ?? (name === 'index.md' ? folder : name);
        const glyph = name === 'log.md' ? 'log' : name === 'index.md' ? 'folder' : 'doc';
        const note = backlinkNote(link);
        return row({ href: hrefFor(catalog, link.path), glyph, title, end: note ? h('span', { class: 'r' }, note) : null });
    }))];
}

function scrollTo(section) {
    if (!section) return;
    requestAnimationFrame(() => document.querySelector(`[data-slug="${CSS.escape(section)}"]`)?.scrollIntoView({ block: 'start' }));
}

export async function render(view, ctx) {
    const { catalog, path, section } = ctx.route;
    const base = `/catalogs/${enc(catalog)}`;
    // Each read's refusal is acted on the moment it comes (ctx.settle: a
    // 401 wipes and signs in without waiting for the other reads); what to
    // draw is decided once all have settled. lastEdit never rejects with a
    // ReadError.
    const settled = await Promise.allSettled([
        ctx.settle(read(`${base}/concepts/${encPath(path)}?full=true`)),
        ctx.settle(backlinks(base, path)),
        lastEdit(base, path),
    ]);
    const refused = settled.filter(outcome => outcome.status === 'rejected').map(outcome => outcome.reason);
    const unexpected = refused.find(reason => !(reason instanceof ReadError));
    if (unexpected) throw unexpected;
    if (refused.length) {
        ctx.drawWorst(refused);
        return;
    }
    const [result, links, edit] = settled.map(outcome => outcome.value);
    if (!ctx.live()) return;
    ctx.freshness(result);
    ctx.freshness(links);
    const concept = result.data;
    const headings = concept.outline ?? [];
    const article = renderBody(concept.body_html ?? '', { serif: true });
    // The page title is drawn above: its heading in the body would repeat it.
    const top = titleHeading(headings, concept.title);
    if (top) article.querySelector(`h1[data-slug="${CSS.escape(top.slug)}"]`)?.remove();
    const description = typeof concept.frontmatter?.description === 'string' ? concept.frontmatter.description : null;
    // fill, not append: the lede and the outline may be null.
    fill(view,
        crumbs(catalog, path, false, { root: ctx.label(), titles: ctx.titles() }),
        pageTitle(concept.title ?? fileName(path), 'concept-title'),
        metaRow(concept, edit),
        lede(description, 'concept-lede'),
        onThisPage(catalog, path, sections(headings, concept.title)),
        article,
        linkedFrom(catalog, links, ctx.label()));
    scrollTo(section);
}
```

- [ ] **Step 3: Run the journeys**

Run: `tools/agentic/gate.sh narrow --test ui_e2e -- read:: --test-threads=2`
Expected: PASS. (`a_concept_shows_its_body_follows_links_and_lists_backlinks` still finds beta's "Settings" in `#outline`, and alpha, whose only heading is its title, draws no outline and no stray "null".)

- [ ] **Step 4: Compare with the mockup, then gates, changelog and commit**

At a phone width, in both schemes, against the mockup's Pages direction, Concept screen, with the Reader direction's serif body and lede. Then `fmt`, `lint`, `test`, `test-ui`, `dylint` (exit 0).

`changelog/<ts>-reader-polish-concept.md`:

```markdown
# Reader: the concept screen

## Changed
- Below the title, a meta row: the `type` and up to three `tags` as pills,
  "Edited 1 h ago by Name" (from the history route; left out offline) and
  the reading time. The frontmatter `description` is a serif lede.
- "On this page" lists every heading but the page title, with the section
  count, whatever the levels.
- The body is set in Literata at 17.5 px with headings in Inter; inline code
  is a small chip, quotes a soft callout, links underlined in the text
  colour. The title heading is not repeated in the body.
- "Linked from · N" lists each linking document once, with "N links"
  when it links more than once; a log shows as "Log" with its entries.

## Fixed
- A concept made of level-1 sections had an empty outline: only the title
  heading is dropped now.
- A document that linked several times was listed once per link.
- A long concept stopped behind "Show the whole concept": the reader always
  reads the whole body (`?full=true`). The button and its notices are gone.

## Tests
- Journeys: level-1 sections and the meta row; three links shown once with a
  count; a long concept whole without a button, offline too, and after a
  lost grant; a late answer leaves a newer screen alone. The journeys that
  held or failed the concept read by URL now name `?full=true`.
```

```bash
git add src tests changelog
git commit -m "feat(read): concept meta row, lede, outline of every section, serif body, whole body, one backlink per document

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 8: Search: plain snippets, Show more, "50+ results"

**Files:**
- Rewrite: `src/http/read/js/screens/search.js`
- Modify: `tests/ui_e2e/read.rs` (new journey after `home_and_search_without_a_connection`)

**Depends on:** Tasks 1 (`snippet_text`), 4 (parts, `notes`, `server_with_catalog`).

`GET /search` has no cursor (spec change 1): "Show more" reads again with `limit` 50 higher, up to the server's 200, and appends the hits past those shown. The order is total (rank, catalog order, path), so a larger answer starts with the hits already drawn.

- [ ] **Step 1: Write the failing journey**

After `home_and_search_without_a_connection`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_shows_more_past_fifty_with_plain_snippets() {
    let server = server_with_catalog("many", &notes(60, "A **bold** note with `code`.")).await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/s/many?q=note", EMAIL, PASSWORD);
        ui.wait_text("#search-count", "50+ results");
        let shown = "() => document.querySelectorAll('#hits a.row').length";
        assert_eq!(ui.eval(shown, json!(null)), json!(50));
        // The snippet is text: no markdown syntax.
        let hits = ui.wait_text("#hits", "A bold note with code.");
        assert!(!hits.contains("**") && !hits.contains('`'), "{hits}");
        ui.click("#search-more");
        ui.wait_text("#search-count", "60 results");
        assert_eq!(ui.eval(shown, json!(null)), json!(60));
        ui.wait_hidden("#search-more");
        // The first fifty were kept, not drawn again.
        assert_eq!(
            ui.eval(
                "() => new Set([...document.querySelectorAll('#hits a.row')].map(a => a.getAttribute('href'))).size",
                json!(null)
            ),
            json!(60)
        );
    })
    .await;
}
```

Run: `tools/agentic/gate.sh narrow --test ui_e2e -- read::search_shows_more --test-threads=2`
Expected: FAIL: the count reads "50 hits" and there is no `#search-more`.

- [ ] **Step 2: Rewrite `search.js`**

```js
// Search in the current catalog: the title, folder and a plain-text snippet
// of each hit, "Show more" when a page fills the limit, and the count
// ("50+ results" while there may be more). It needs the server: offline it
// says so and points at what is available offline.
import { h } from '/ui/js/core/dom.js';
import { read, enc, KIND, ReadError } from '../api.js';
import { hrefs, hrefFor } from '../route.js';
import { searchForm, message, row } from '../parts.js';
import { docLabel, folderTrail } from '../text.js';

// Hits per "page", and the most one search returns (src/mcp/search.rs,
// MAX_LIMIT): /search has no cursor, so "Show more" asks for more at once.
const PAGE = 50;
const MOST = 200;

function hitRow(hit) {
    return row({
        href: hrefFor(hit.catalog, hit.path), glyph: 'doc',
        title: docLabel(hit), sub: folderTrail(hit.path) || null, note: hit.snippet_text,
    });
}

function counted(n, full) {
    if (full) return `${n}+ results`;
    return `${n} ${n === 1 ? 'result' : 'results'}`;
}

export async function render(view, ctx) {
    const { catalog, query } = ctx.route;
    view.append(searchForm(catalog, query));
    if (!query) return;
    const url = limit => `/search?q=${enc(query)}&catalogs=${enc(catalog)}&limit=${limit}`;
    let limit = PAGE;
    let result;
    try {
        result = await read(url(limit));
    } catch (error) {
        if (error.kind !== KIND.UNAVAILABLE) throw error;
        if (!ctx.live()) return;
        view.append(message('Search needs a connection.',
            h('a', { href: hrefs.home(), id: 'offline-list-link' }, 'See what is available offline'), 'search-offline'));
        return;
    }
    if (!ctx.live()) return;
    const count = h('p', { class: 'hint', id: 'search-count' });
    const hits = h('div', { class: 'rows', id: 'hits' });
    const more = h('button', { type: 'button', class: 'more-button', id: 'search-more' }, 'Show more');
    const capped = message('Refine the search to see results past these.', null, 'search-capped');
    let shown = 0;
    // Draws the hits of `answer` past those shown, and what may come next.
    const draw = answer => {
        const found = answer.data.hits;
        hits.append(...found.slice(shown).map(hitRow));
        shown = found.length;
        const full = found.length >= limit;
        count.textContent = counted(found.length, full);
        more.hidden = !full || limit >= MOST;
        capped.hidden = !(full && limit >= MOST);
    };
    more.addEventListener('click', async () => {
        more.disabled = true;
        limit = Math.min(limit + PAGE, MOST);
        try {
            const answer = await read(url(limit));
            if (!ctx.live()) return;
            draw(answer);
        } catch (error) {
            if (!(error instanceof ReadError)) throw error;
            limit -= PAGE;
            if (error.kind !== KIND.UNAVAILABLE) {
                await ctx.fail(error);
                return;
            }
            if (ctx.live() && !document.getElementById('more-offline')) {
                more.after(message('More results need a connection.', null, 'more-offline'));
            }
        } finally {
            more.disabled = false;
        }
    });
    view.append(count, hits, more, capped);
    draw(result);
}
```

- [ ] **Step 3: Run the journeys**

Run: `tools/agentic/gate.sh narrow --test ui_e2e -- read:: --test-threads=2`
Expected: PASS (`home_lists_recent_changes_and_search_finds_a_concept` still finds "Alpha" in `#hits` and opens it through `#hits a`; no stray "null").

- [ ] **Step 4: Gates, changelog and commit**

Run `fmt`, `lint`, `test`, `test-ui`, `dylint` (exit 0).

`changelog/<ts>-reader-polish-search.md`:

```markdown
# Reader: search

## Changed
- Each hit shows a document icon, its title, its folder and a plain-text
  snippet (the server's `snippet_text`).
- A page that fills the limit offers "Show more", which reads again with 50
  more (up to the server's 200, then a hint to refine the search), and the
  count reads "50+ results" rather than a flat "50 hits".

## Fixed
- Snippets showed raw markdown (`**`, backticks, link syntax).
- Search stopped silently at 50 hits.

## Tests
- Journey: 60 matching notes show 50 with "50+ results", then all 60 after
  Show more, each once, with no markdown in the snippets.
```

```bash
git add src tests changelog
git commit -m "feat(read): search with plain snippets and Show more

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 9: Account and catalog picker restyle, theme choice

**Files:**
- Create: `src/http/read/js/theme.js`
- Modify: `src/http/read/index.html` (one `<script>` in the head)
- Rewrite: `src/http/read/js/screens/me.js`
- Modify: `src/http/read/js/screens/catalogs.js` (the list's rows)
- Modify: `tests/ui_e2e/read.rs` (new journey after `an_account_without_an_email_shows_no_null`)

**Depends on:** Task 4 (parts, header). `catalogs.test.mjs` keeps passing: buttons keep `dataset.catalog`, their text still holds the label (else the name), and the doubles cover every part used.

The theme choice (spec change 4) is a device preference, not account data: sign-out and "Delete offline copies" leave it.

- [ ] **Step 1: Write the failing journey**

After `an_account_without_an_email_shows_no_null`:

```rust
/// `[data-theme on <html>, the light theme-color meta's colour]` now.
const THEME_STATE: &str = "() => [document.documentElement.dataset.theme ?? null, \
    document.querySelector('meta[name=theme-color][media*=light]').content]";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_theme_choice_overrides_the_system_and_survives_a_reload() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_text("#account-user", "reader");
        assert_eq!(ui.eval(THEME_STATE, json!(null)), json!([null, "#ffffff"]));
        ui.click("#theme-dark");
        // Dark even where the system is light: the system bar follows.
        assert_eq!(ui.eval(THEME_STATE, json!(null)), json!(["dark", "#191919"]));
        ui.reload_reader();
        ui.wait_text("#account-user", "reader");
        assert_eq!(ui.eval(THEME_STATE, json!(null)), json!(["dark", "#191919"]));
        assert_eq!(
            ui.eval("() => document.querySelector('#theme-dark').getAttribute('aria-pressed')", json!(null)),
            json!("true")
        );
        ui.click("#theme-system");
        assert_eq!(ui.eval(THEME_STATE, json!(null)), json!([null, "#ffffff"]));
    })
    .await;
}
```

Run: `tools/agentic/gate.sh narrow --test ui_e2e -- read::a_theme_choice --test-threads=2`
Expected: FAIL: no `#theme-dark`.

- [ ] **Step 2: Write `theme.js`**

```js
// The reader's colour theme: the system's (no data-theme on <html>), or a
// choice of light or dark from the Account screen, kept in localStorage.
// A classic script loaded in <head> after the theme-color metas, so the
// stored choice applies before the first paint, and the system bars follow
// it. The colours are read.css's --bg.
(() => {
    const KEY = 'okf.read.theme';
    const BARS = Object.freeze({ light: '#ffffff', dark: '#191919' });
    const CHOICES = Object.freeze(['system', 'light', 'dark']);

    function stored() {
        try { return localStorage.getItem(KEY); } catch { return null; }
    }

    // Sets data-theme and the theme-color metas for `choice` (anything but
    // 'light' or 'dark' means the system's).
    function apply(choice) {
        const root = document.documentElement;
        const fixed = choice === 'light' || choice === 'dark';
        if (fixed) root.dataset.theme = choice;
        else delete root.dataset.theme;
        for (const meta of document.querySelectorAll('meta[name="theme-color"]')) {
            const scheme = meta.media.includes('dark') ? 'dark' : 'light';
            meta.content = BARS[fixed ? choice : scheme];
        }
    }

    self.READ_THEME = Object.freeze({
        CHOICES,
        // 'system', 'light' or 'dark'.
        current() {
            const choice = stored();
            return choice === 'light' || choice === 'dark' ? choice : 'system';
        },
        choose(choice) {
            try {
                if (choice === 'light' || choice === 'dark') localStorage.setItem(KEY, choice);
                else localStorage.removeItem(KEY);
            } catch { /* storage blocked: the choice lasts for this visit only */ }
            apply(choice);
        },
    });
    apply(stored());
})();
```

In `src/http/read/index.html`, after the two `theme-color` metas:

```html
    <script src="/read/js/theme.js"></script>
```

- [ ] **Step 3: Rewrite `me.js`**

```js
// Account: who is signed in, until when, the theme choice, sign-out (which
// deletes every offline copy and the remembered catalog first, even if the
// server cannot be reached), and a local "Delete offline copies" that works
// with no connection at all. Both delete through main.js (`ctx.signOut`,
// `ctx.forget`).
import { h, fill } from '/ui/js/core/dom.js';
import { when } from '/ui/js/core/format.js';
import { message, pageTitle, sectionHead } from '../parts.js';
import { avatarClass, initials } from '../avatar.js';

const LABELS = Object.freeze({ system: 'System', light: 'Light', dark: 'Dark' });

function deleteOffline(view, ctx) {
    const button = h('button', { type: 'button', class: 'secondary', id: 'delete-offline' }, 'Delete offline copies');
    button.addEventListener('click', async () => {
        button.disabled = true;
        await ctx.forget();
        view.append(message('Offline copies are deleted from this device.', null, 'offline-deleted'));
    });
    return button;
}

// System, Light, Dark (theme.js); the current one pressed.
function appearance() {
    const theme = self.READ_THEME;
    const chips = theme.CHOICES.map(choice => h('button', {
        type: 'button', class: 'chip', id: `theme-${choice}`,
        'aria-pressed': String(theme.current() === choice),
        onclick: () => {
            theme.choose(choice);
            for (const chip of chips) chip.setAttribute('aria-pressed', String(chip.id === `theme-${choice}`));
        },
    }, LABELS[choice]));
    return [sectionHead('Appearance'), h('div', { class: 'chips', role: 'group', 'aria-label': 'Appearance' }, chips)];
}

export async function render(view, ctx) {
    const session = ctx.state.session;
    if (!session) {
        fill(view, pageTitle('Account'),
            message('Account details and sign-out need a connection.', null, 'account-offline'),
            appearance(),
            h('div', { class: 'actions' }, deleteOffline(view, ctx)));
        return;
    }
    const button = h('button', { type: 'button', id: 'sign-out' }, 'Sign out');
    button.addEventListener('click', async () => {
        button.disabled = true;
        // On success the page has left for sign-in.
        if (await ctx.signOut(session.csrf)) return;
        button.disabled = false;
        view.append(message('The server could not be reached to end the session. Offline copies are deleted.', null, 'sign-out-failed'));
    });
    // fill, not append: the email is null for an account without one.
    fill(view,
        pageTitle('Account'),
        h('div', { class: 'card' },
            h('span', { class: `avatar ${avatarClass(session.user)}`, 'aria-hidden': 'true' }, initials(session.user)),
            h('span', { class: 'tx' },
                h('span', {}, 'Signed in as ', h('strong', { id: 'account-user' }, session.user)),
                session.email ? h('span', { class: 's', id: 'account-email' }, session.email) : null)),
        h('p', { class: 'hint', id: 'account-expires' }, `This session ends by ${when(session.expires_at)}.`),
        appearance(),
        h('div', { class: 'actions' }, button, deleteOffline(view, ctx)));
}
```

(`an_account_without_an_email_shows_no_null` renders this with a context holding only `state`, and `READ_THEME` exists in the page: it still finds "Signed in as reader" and no "null".)

- [ ] **Step 4: The picker's rows**

In `src/http/read/js/screens/catalogs.js`, import `pageTitle` from `../parts.js` beside `message`, replace `view.append(h('h1', {}, 'Catalogs'));` with `view.append(pageTitle('Catalogs'));`, and replace the list with:

```js
    view.append(h('div', { class: 'rows', id: 'catalog-list' }, found.map(catalog => {
        const label = catalog.label ?? catalog.name;
        return h('button', {
            type: 'button', class: 'row', dataset: { catalog: catalog.name },
            onclick: () => { ctx.choose(catalog.name); location.hash = hrefs.home(); },
        },
        h('span', { class: 'mark', 'aria-hidden': 'true' }, ([...label.trim()][0] ?? '?').toUpperCase()),
        h('span', { class: 'tx' },
            h('span', { class: 't' }, label),
            catalog.description ? h('span', { class: 's' }, catalog.description) : null),
        typeof catalog.concepts === 'number' ? h('span', { class: 'r' }, String(catalog.concepts)) : null);
    })));
```

Run: `tools/agentic/gate.sh worker`
Expected: PASS (`catalogs.test.mjs` included).

- [ ] **Step 5: Run the journeys, compare, gates, changelog, commit**

Run: `tools/agentic/gate.sh narrow --test ui_e2e -- read:: --test-threads=2` (PASS), compare Account and the picker at a phone width in both schemes, then `fmt`, `lint`, `test`, `test-ui`, `dylint` (exit 0).

`changelog/<ts>-reader-polish-account.md`:

```markdown
# Reader: Account, the catalog picker and a theme choice

## Added
- Account has an Appearance choice: System (the default), Light or Dark. It
  sets the page's theme and the system bars' colour, applies before the
  first paint (`js/theme.js`), and stays on the device across sign-outs.

## Changed
- Account restyled: the user with an avatar, the email, when the session
  ends, Sign out and Delete offline copies, as before.
- The catalog picker lists each catalog with its mark, label, description
  and page count.

## Tests
- Journey: a theme choice overrides the system, survives a reload, and
  System gives it back.
```

```bash
git add src tests changelog
git commit -m "feat(read): Account and picker restyle, theme choice

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 10: Visual check on a phone, and a before/after page

**Files:**
- Modify: `tests/ui_e2e/read.rs` (an ignored test at the end that serves a sample catalog)

**Depends on:** Tasks 5 to 9 merged into the milestone branch.

The journeys check behaviour; this task checks the look against the mockup, on a real phone, in both themes, and produces a page with each screen before and after for the review. Screenshots are working files: keep them in the session scratchpad, never in the repository.

- [ ] **Step 1: A sample catalog to look at**

At the end of `tests/ui_e2e/read.rs`:

```rust
/// The sample catalog of the visual check: placeholder content shaped like
/// the mockup's (folders with descriptions, a long concept with tags,
/// quotes, code and links, several actors and kinds of change).
fn visual_sample() -> Vec<(String, String)> {
    let mut long = String::from(
        "---\ntype: system\ntitle: Kubernetes cluster\ndescription: The home cluster: three control-plane nodes and five workers, deployed from git.\n\
         tags: [k8s, infra, platform]\n---\n# Kubernetes cluster\n\n\
         > This is the test cluster. Production runs separately: see [Production deployment](production.md).\n\n\
         The cluster runs three control-plane nodes and five workers. Workloads are deployed from the `infra` repository.\n\n",
    );
    for section in ["Nodes", "Deployments", "Ingress", "Certificates", "Upgrades", "Incidents"] {
        long.push_str(&format!(
            "## {section}\n\nEach node is a virtual machine. Upgrades go one node at a time, with `kubectl drain` first, \
             and the control plane last. See [Image updater](image-updater.md) for how new builds roll out.\n\n"
        ));
    }
    [
        ("index.md", "---\ntitle: Example Notes\ndescription: What the team keeps.\n---\n# Example Notes\n".to_string()),
        ("infra/index.md", "---\ntitle: Infrastructure\ndescription: Shared platforms that services run on.\n---\n# Infrastructure\n".to_string()),
        ("infra/network/index.md", "---\ntitle: Network\ndescription: Routers, DNS, the edge.\n---\n# Network\n".to_string()),
        ("infra/network/dns.md", "---\ntitle: Internal DNS\ndescription: Names under example.internal.\n---\n# Internal DNS\n\nServes example.internal.\n".to_string()),
        ("infra/kubernetes.md", long),
        ("infra/image-updater.md", "---\ntitle: Image updater\ndescription: Moves deployments to the newest build.\n---\n# Image updater\n\nIt watches the registry and links back to [the cluster](kubernetes.md).\n".to_string()),
        ("infra/production.md", "---\ntitle: Production deployment\ndescription: How a release reaches production.\n---\n# Production deployment\n\nSee [the cluster](kubernetes.md), [its nodes](kubernetes.md#nodes) and [upgrades](kubernetes.md#upgrades).\n".to_string()),
        ("infra/old-vpn.md", "---\ntitle: Old VPN notes\n---\n# Old VPN notes\n".to_string()),
        ("processes/index.md", "---\ntitle: Processes\ndescription: How work gets done here.\n---\n# Processes\n".to_string()),
        ("processes/release.md", "---\ntype: process\ntitle: Release process\ndescription: How a release is cut, tagged and shipped.\n---\n# Release process\n\n1. Tag.\n2. Ship.\n".to_string()),
    ]
    .into_iter()
    .map(|(path, text)| (path.to_string(), text))
    .collect()
}

/// Serves [`visual_sample`] for the visual check (plan Task 10), with later
/// rounds by other actors so Home shows groups, edits and a deletion. Not a
/// journey: run it by hand (it is ignored otherwise); it prints the URL and
/// serves for `READER_VISUAL_MINUTES` (default 30).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "serves a sample catalog for the manual visual check"]
async fn serve_a_sample_catalog_for_the_visual_check() {
    let server = server_with_catalog("notes", &visual_sample()).await;
    server.set_catalog_label("notes", "Example Notes").await;
    let mut entries = visual_sample();
    if let Some((_, text)) = entries
        .iter_mut()
        .find(|(path, _)| path.as_str() == "processes/release.md")
    {
        text.push_str("3. Announce.\n");
    }
    let edited: Vec<(&str, &str)> = entries.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    publish_as(&server, "sam", files(&edited)).await;
    let kept: Vec<(&str, &str)> = edited.iter().copied().filter(|(path, _)| *path != "infra/old-vpn.md").collect();
    publish_as(&server, "alex", files(&kept)).await;
    let minutes = std::env::var("READER_VISUAL_MINUTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30);
    eprintln!(
        "visual check: {}/read/ (sign in as {EMAIL}, password {PASSWORD}); serving for {minutes} min",
        server.base
    );
    tokio::time::sleep(Duration::from_secs(minutes * 60)).await;
}

/// Imports `entries` over the `notes` catalog as `actor`, as a publish would.
async fn publish_as(server: &TestServer, actor: &str, entries: okf_mcp::archive::Files) {
    let options = ImportOptions {
        replace: true,
        scope: ImportScope::Everything,
        expect_generation: None,
        expect_catalog_id: None,
        actor,
        audit: &AuditActor::cli(),
        now: Utc::now(),
    };
    crate::support::server::applied(&server.app, async {
        import(&server.db.pool, "notes", entries, &options)
            .await
            .expect("import");
    })
    .await;
}
```

Run: `tools/agentic/gate.sh narrow --test ui_e2e -- read::serve_a_sample --test-threads=2` (it is ignored, so it is listed and skipped).
Expected: `1 ignored`. Then `tools/agentic/gate.sh lint` (exit 0).

- [ ] **Step 2: Screenshots after, on the phone**

With an Android phone on USB with debugging on (`adb devices` lists it), in one terminal:

```bash
export TEST_PG_URL=$(scripts/test-pg.sh up okf-mcp-test-pg-polish)
AGENTIC_TIMEOUT_NARROW=40m READER_VISUAL_MINUTES=30 \
  tools/agentic/gate.sh narrow --test ui_e2e -- read::serve_a_sample --ignored --nocapture
```

It prints `visual check: http://127.0.0.1:<port>/read/ ...`. In another terminal:

```bash
adb reverse tcp:<port> tcp:<port>
adb shell am start -a android.intent.action.VIEW -d "http://127.0.0.1:<port>/read/"
```

Sign in, open Home once so the worker installs, then for each theme (`adb shell cmd uimode night no`, then `yes`) capture each screen with `adb exec-out screencap -p > "$SCRATCH/visual/after-<screen>-<theme>.png"`: Home (top, and an expanded group), Browse root, `infra`, the Kubernetes concept (top, the outline open, Linked from), Search for `the`, Account, and the picker. `127.0.0.1` through `adb reverse` is a secure context, so the worker runs as it does in production. Restore the phone's theme afterwards.

- [ ] **Step 3: Compare with the mockup**

Open the mockup on the same phone (`adb push docs/superpowers/specs/assets/2026-11-21-reader-polish-mockups.html /sdcard/Download/` and open it from Files) and compare each screen with its Pages screen (Home with the Activity direction, the concept body with the Reader direction): type, spacing, colours, icons, and the navigation bar's colour in both themes (bug 4). Fix any difference in `read.css`, in the section of that screen, with one commit per screen, and capture again.

- [ ] **Step 4: Screenshots before**

```bash
base=$(git merge-base origin/main HEAD)
git worktree add "$SCRATCH/before" "$base"
cp tests/ui_e2e/read.rs "$SCRATCH/read-after.rs"
```

In `$SCRATCH/before/tests/ui_e2e/read.rs`, append `server_with_catalog`, `visual_sample`, `serve_a_sample_catalog_for_the_visual_check` and `publish_as` from `$SCRATCH/read-after.rs` (the older file lacks them), run the same command from `$SCRATCH/before`, and capture the same screens as `before-<screen>-<theme>.png`. Then `git worktree remove --force "$SCRATCH/before"`.

- [ ] **Step 5: The before/after page**

Write this script to `$SCRATCH/before-after.sh` (not to the repository) and run `bash "$SCRATCH/before-after.sh" "$SCRATCH/visual" > "$SCRATCH/visual/before-after.html"`:

```bash
#!/usr/bin/env bash
# Writes an HTML page pairing before-<name>.png and after-<name>.png from
# the directory $1, images inlined, to stdout.
set -euo pipefail
dir=$1
cat <<'HEAD'
<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Reader before and after</title>
<style>
:root { color-scheme: light dark; --bg: #ffffff; --text: #37352f; --muted: #787774; }
@media (prefers-color-scheme: dark) { :root { --bg: #191919; --text: #e3e2e0; --muted: #9b9a97; } }
body { margin: 0 auto; max-width: 960px; padding: 16px; background: var(--bg); color: var(--text); font: 15px/1.5 system-ui, sans-serif; }
h2 { font-size: 15px; margin: 28px 0 8px; }
.pair { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
figure { margin: 0; } figcaption { color: var(--muted); font-size: 13px; }
img { width: 100%; height: auto; border-radius: 12px; }
</style></head><body><h1>Reader polish: before and after</h1>
HEAD
for after in "$dir"/after-*.png; do
    name=${after##*/after-}
    name=${name%.png}
    before=$dir/before-$name.png
    printf '<h2>%s</h2><div class="pair">' "$name"
    if [ -f "$before" ]; then
        printf '<figure><img alt="before" src="data:image/png;base64,%s"><figcaption>Before</figcaption></figure>' "$(base64 -w0 "$before")"
    else
        printf '<figure><figcaption>No before screenshot</figcaption></figure>'
    fi
    printf '<figure><img alt="after" src="data:image/png;base64,%s"><figcaption>After</figcaption></figure></div>\n' "$(base64 -w0 "$after")"
done
printf '</body></html>\n'
```

Hand the page to the coordinator, who publishes it for review before the milestone merges. Check first that no screenshot shows anything but the sample catalog (no notification shade, no other app).

- [ ] **Step 6: Full gates, changelog and commit**

Run `fmt`, `lint`, `test`, `test-ui`, `dylint` and `scripts/test-pg.sh down okf-mcp-test-pg-polish`.

`changelog/<ts>-reader-polish-visual-check.md`:

```markdown
# Reader: the visual check

## Added
- `tests/ui_e2e/read.rs` `serve_a_sample_catalog_for_the_visual_check`, an
  ignored test that serves a sample catalog (placeholder content shaped like
  the mockup's) so the reader can be opened on a phone through
  `adb reverse`.

## Changed
- <each read.css fix from the comparison, by screen>

## Docs
- Each screen was compared with the mockup on a phone in light and dark,
  and a before/after page went to review.
```

```bash
git add tests src changelog
git commit -m "test(read): sample catalog for the visual check; style fixes from it

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: Reader reference and the milestone changelog

**Files:**
- Modify: `docs/reference/reader.md`
- Create: `changelog/<ts>-reader-polish.md`

**Depends on:** Task 10.

- [ ] **Step 1: Update `docs/reference/reader.md`**

In "Screens and routes", replace the rows and paragraphs that changed:
- `#/`: "Home: the install hint (iPhone and iPad browsers only), a search box with the catalog's page count, "Jump back in" (the three concepts read last), and the activity timeline: filter chips (All, Created, Edited, Deleted, By me), day headings, and changes grouped by actor and batch (the same message, less than 2 minutes apart), with "+N more" and "Show older". When the server cannot be reached or the worker answered from its copy, every offline copy is listed under "Available offline"."
- `#/t/...`: "One folder: breadcrumbs (the catalog's label, then folder titles), its title and description, an About row that opens its `index.md`, its subfolders (description and page count), its concepts (description) under "Pages · N", and a Log row for its `log.md`."
- `#/s/...`: "Search results in that catalog: title, folder and a plain-text snippet. "Show more" adds 50 more, up to 200; "50+ results" means there may be more."
- `#/v/...`: "A concept: breadcrumbs, title, a meta row (type, tags, the last edit, reading time), its description as a lede, "On this page" (every heading but the title), the body in the serif reading type, and "Linked from" with one row per document. The reader always reads the whole body. `?s=<slug>` scrolls to a heading."
- `#/d/...`: "A folder's `index.md` (shown as About), a `log.md` (Log) or `AGENTS.md`, read as a document."
- Delete the paragraph about a shortened concept and the "Show the whole concept" button.
- Account: add "and an Appearance choice (System, Light, Dark), kept on the device".

In "For maintainers": the files list adds `icons.svg`, `fonts/` and `js/` `icons.js`, `theme.js`, `text.js`, `timeline.js`, `outline.js`, `backlinks.js`, `avatar.js`; "Styling" says `read.css` holds the tokens (light, dark and `data-theme`), maps Pico's variables onto them, and has one section per screen; the fonts are built by `scripts/subset-reader-fonts.sh` (check them with `sha256sum -c scripts/reader-fonts.sha256`); the tests paragraph names `tools/agentic/gate.sh worker` and the new Node test files; the manual checks add the visual check (Task 10 of `docs/superpowers/plans/2026-11-21-reader-polish.md`); the design list adds `docs/superpowers/specs/2026-11-21-reader-polish-design.md`.

- [ ] **Step 2: The milestone changelog**

`changelog/<ts>-reader-polish.md`, summarising the milestone for its merge (the per-task entries keep the detail):

```markdown
# Mobile reader polish

The reader at `/read/` gets a Notion-like design and six fixes found on a
phone. Design: `docs/superpowers/specs/2026-11-21-reader-polish-design.md`;
plan: `docs/superpowers/plans/2026-11-21-reader-polish.md`.

## Added
- Server: titles and descriptions on recent changes, descriptions on the
  tree, `snippet_text` on search hits; fonts from the server's own origin
  in the CSP.
- Self-hosted Inter and Literata (Latin subsets, OFL), an icon sprite, and
  design tokens for light, dark and a theme choice on Account.
- Home: page count, Jump back in, an activity timeline with filters, day
  headings, batched groups and Show older.
- Folder: About, described folders with counts, described concepts, a Log
  row. Concept: meta row, lede, On this page, serif body, one backlink row
  per document. Search: plain snippets and Show more.
- `tools/agentic/gate.sh worker` for the reader's Node tests.

## Fixed
- Screens kept the previous screen's scroll position.
- Backlinks repeated a document once per link.
- A concept of level-1 sections had an empty outline.
- The system navigation bar was green.
- Search showed raw markdown and stopped silently at 50.
- Long concepts stopped behind "Show the whole concept".

## Tests
- Node tests for the timeline, outline, backlinks, avatars and text; new
  journeys for each fix and for the grouped import, Show older, Jump back
  in, the header label and the theme. A sample catalog for the visual
  check on a phone.
```

- [ ] **Step 3: Gates and commit**

Run `tools/agentic/gate.sh fmt` (the docs are not formatted, but a stray edit would be caught) and `tools/agentic/gate.sh lint`.

```bash
git add docs/reference/reader.md changelog
git commit -m "docs(read): reader reference and the polish milestone changelog

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Self-review notes

- **Spec coverage.** Chrome: header label, mark and picker (Task 4), tab icons (4), scroll reset (4), system bars (2). Home: search with count, Jump back in, chips, day headings, groups by actor and batch, "+N more", single rows, struck-through deletions, Show older, the offline list (5). Folder: breadcrumbs with titles, lede, About, folders with description and count, Pages · N with descriptions, Log row (6). Concept: meta row, lede, On this page for every heading but the title, serif body, inline code chip, callout quotes, text-colour links, always `?full=true`, Linked from · N with counts and log entries (7). Search: plain snippets, Show more, "50+ results" (8). Account restyle (9). Server changes (1). Shell (2, automatic through `READ_ASSETS`, tested). Bugs 1 to 6: Tasks 4, 7, 7, 2, 1 and 8, 7. Testing: Rust unit tests (1, 2, 4), Node tests (3, 6), journeys (4 to 9), visual check (10).
- **Ids kept:** every id in "Rules for every task" is still drawn; `#recent` holds the timeline's document links; `#offline-list` is drawn whenever Home lists offline copies, as before.
- **Names used across tasks:** `row({ href, glyph, tint, title, sub, note, end, id, cls })`, `sectionHead(title, { count, id })`, `pill(text, kind)`, `pageTitle(text, id)`, `lede(text, cls)`, `crumbs(catalog, path, isFolder, { root, titles })`, `renderBody(html, { serif })`, `searchForm(catalog, query, placeholder)` (Task 4); `ctx.summary()`, `ctx.label()`, `ctx.titles()`, `ctx.rememberTitles(map)` (Task 4); `timeline`, `FILTERS`, `GROUP_PREVIEW`, `changeKind`, `verb`, `object`, `summary`, `dayKey`, `dayLabel`, `BATCH_GAP_MS` (Task 3); `sections`, `titleHeading`, `dedupeBacklinks`, `backlinkNote`, `initials`, `avatarClass`, `AVATAR_COLOURS`, `fileName`, `folderTrail`, `docLabel`, `pages`, `readingMinutes`, `shortAgo`, `clock` (Task 3); `folderListing` (`folders: [{ name, path, title, description, pages }]`, `index`, `log`, `concepts`, `label`) and `folderTitles` (Task 6); `server_with_catalog`, `notes` (Task 4). The doubles in `tree-doubles/` mirror the parts' signatures.

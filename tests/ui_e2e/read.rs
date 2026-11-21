//! The mobile reader at `/read/` (reader design): sign-in through the
//! console's session, the catalog picker, the tree, concepts and their
//! links, search, offline reading from the service worker's caches, and
//! what a revoked session, a removed grant or an unlock does to them.

use std::time::{Duration, Instant};

use chrono::Utc;
use okf_mcp::config::Config;
use okf_mcp::store::audit::AuditActor;
use okf_mcp::store::ceiling::{self, NewEntitlement};
use okf_mcp::store::import::{import, ImportOptions, ImportScope};
use okf_mcp::store::keys::Access;
use okf_mcp::store::user_sessions;
use serde_json::json;

use crate::harness::{drive, Ui};
use crate::support::server::{files, TestServer};

const PASSWORD: &str = "violet-lantern-orbit-42";
const EMAIL: &str = "reader@example.com";

/// Every cache's entry count, by cache name.
const CACHE_COUNTS: &str = "async () => { const out = {}; \
    for (const name of await caches.keys()) out[name] = (await (await caches.open(name)).keys()).length; \
    return out; }";

/// Reads alpha from the page, through the worker, which keeps a copy in
/// `read-content` and records the account in `read-owner`. Returns the
/// answer's status.
const READ_ALPHA: &str = "async () => \
    (await fetch('/api/v1/catalogs/main/concepts/systems/alpha.md')).status";

/// The catalog the reader remembers in `localStorage` (null for none).
const REMEMBERED_CATALOG: &str = "() => localStorage.getItem('okf.read.catalog')";

/// The status `GET /api/v1/session` answers the page with now.
const SESSION_STATUS: &str = "async () => (await fetch('/api/v1/session')).status";

/// `main`: a root index, a `systems` folder with its index and two
/// concepts (alpha links to beta's Settings section), plus `other`.
fn main_files() -> okf_mcp::archive::Files {
    files(&[
        ("index.md", "# Main\n\n- [Systems](systems/index.md)\n"),
        ("log.md", "# Log\n"),
        (
            "systems/index.md",
            "# Systems\n\n- [Alpha](alpha.md)\n- [Beta](beta.md)\n",
        ),
        (
            "systems/alpha.md",
            "---\ntype: system\ntitle: Alpha\n---\n# Alpha\n\nSee [Beta settings](beta.md#settings).\n\nThe word marmalade.\n",
        ),
        ("systems/empty.md", "---\ntype: system\ntitle: Empty\n---\n"),
        (
            "systems/beta.md",
            "---\ntype: system\ntitle: Beta\n---\n# Beta\n\nIntro.\n\n## Settings\n\nTwo settings.\n",
        ),
    ])
}

/// A server with `main` and `other`, sign-in routes on, and a user
/// `reader` who can read both.
///
/// # Returns
/// The running server and the reader's user id.
async fn server() -> (TestServer, String) {
    server_with_outline_limit(Config::default().concept_outline_chars).await
}

/// [`server`] with `concept_outline_chars` set, so a longer concept comes
/// back as its outline only.
///
/// # Arguments
/// * `outline_chars` - the longest body the server renders without `full`.
async fn server_with_outline_limit(outline_chars: usize) -> (TestServer, String) {
    let server = TestServer::start_with(
        vec![
            ("main", main_files()),
            (
                "other",
                files(&[
                    ("index.md", "# Other\n"),
                    ("notes.md", "---\ntitle: Notes\n---\n# Notes\n"),
                ]),
            ),
        ],
        // `public_url` is the server's own address, so `/signin` exists and
        // the session cookie belongs to the host the browser uses.
        |base| Config {
            public_url: Some(base.to_string()),
            concept_outline_chars: outline_chars,
            ..Config::default()
        },
    )
    .await;
    let id = server.user_with_password("reader", EMAIL, PASSWORD).await;
    crate::support::server::applied(&server.app, async {
        for catalog in ["main", "other"] {
            ceiling::entitle(
                &server.db.pool,
                &NewEntitlement {
                    user: "reader",
                    catalog,
                    access: Access::Read,
                },
                &AuditActor::cli(),
                Utc::now(),
            )
            .await
            .expect("entitle");
        }
    })
    .await;
    (server, id)
}

/// A server with one catalog `name` holding `entries`, sign-in routes on,
/// and the user `reader` entitled to read it.
///
/// # Arguments
/// * `name` - the catalog.
/// * `entries` - `(path, text)` of each file.
async fn server_with_catalog(name: &str, entries: &[(String, String)]) -> TestServer {
    server_with_catalog_and_reader(name, entries).await.0
}

/// [`server_with_catalog`], and the reader's user id as well.
async fn server_with_catalog_and_reader(
    name: &str,
    entries: &[(String, String)],
) -> (TestServer, String) {
    let borrowed: Vec<(&str, &str)> = entries
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let server = TestServer::start_with(vec![(name, files(&borrowed))], |base| Config {
        public_url: Some(base.to_string()),
        ..Config::default()
    })
    .await;
    let id = server.user_with_password("reader", EMAIL, PASSWORD).await;
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
    (server, id)
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

/// Signs in at the reader's account screen, waits for the worker, and has
/// it cache alpha, so there is an offline copy for sign-out to delete.
fn signed_in_with_a_copy(ui: &Ui) {
    ui.sign_in_reader("/me", EMAIL, PASSWORD);
    ui.wait_text("#account-user", "reader");
    ui.wait_worker();
    assert_eq!(ui.eval(READ_ALPHA, json!(null)), json!(200));
    let counts = ui.eval(CACHE_COUNTS, json!(null));
    for name in ["read-content", "read-owner"] {
        assert!(
            counts.get(name).is_some_and(|count| count != 0),
            "{name} is empty before sign-out: {counts}"
        );
    }
}

/// Asserts that no cache of the signed-in account's reads is left.
fn assert_no_offline_copies(ui: &Ui) {
    let counts = ui.eval(CACHE_COUNTS, json!(null));
    for name in ["read-content", "read-meta", "read-owner"] {
        assert!(
            counts.get(name).is_none(),
            "{name} survived sign-out: {counts}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signing_in_returns_to_the_route_and_signing_out_wipes_the_caches() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        signed_in_with_a_copy(ui);
        // Sign-in sent the browser back to the route it started from.
        assert_eq!(ui.eval("() => location.hash", json!(null)), json!("#/me"));
        // Opening a catalog's tree remembers it; back to the account.
        ui.eval("() => { location.hash = '#/t/main'; }", json!(null));
        ui.wait_visible("#screen-tree");
        assert_eq!(ui.eval(REMEMBERED_CATALOG, json!(null)), json!("main"));
        ui.eval("() => { location.hash = '#/me'; }", json!(null));
        ui.wait_visible("#sign-out");
        ui.click("#sign-out");
        ui.wait_visible("#signin-email");
        assert_no_offline_copies(ui);
        // The next person to sign in here never sees this one's catalog.
        assert_eq!(ui.eval(REMEMBERED_CATALOG, json!(null)), json!(null));
        // The server ended the session, not only the page.
        assert_eq!(ui.eval(SESSION_STATUS, json!(null)), json!(401));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reads_and_catalogs_while_signing_out_do_not_outlive_it() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        signed_in_with_a_copy(ui);
        ui.hold_next("DELETE", "/api/v1/session");
        ui.click("#sign-out");
        // The copies are deleted and the DELETE is out, held.
        let deadline = Instant::now() + Duration::from_secs(10);
        while ui.held_matches() == 0 {
            assert!(Instant::now() < deadline, "sign-out never sent its DELETE");
            std::thread::sleep(Duration::from_millis(50));
        }
        // A read while the DELETE is held is still the account's, so the
        // worker caches it again after the first wipe.
        assert_eq!(ui.eval(READ_ALPHA, json!(null)), json!(200));
        // Opening a catalog meanwhile shows it, but does not remember it.
        ui.eval("() => { location.hash = '#/t/main'; }", json!(null));
        ui.wait_visible("#screen-tree");
        assert_eq!(ui.eval(REMEMBERED_CATALOG, json!(null)), json!(null));
        // The page leaves once the DELETE answers (`release_held` would
        // wait on a flag of the page it leaves).
        ui.eval("() => window.releaseHeld()", json!(null));
        ui.wait_visible("#signin-email");
        assert_eq!(ui.eval(REMEMBERED_CATALOG, json!(null)), json!(null));
        // Sign-out wiped again once the server confirmed it.
        assert_no_offline_copies(ui);
        assert_eq!(ui.eval(SESSION_STATUS, json!(null)), json!(401));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signing_out_without_a_connection_still_deletes_the_offline_copies() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        signed_in_with_a_copy(ui);
        let offline = ui.go_offline();
        ui.click("#sign-out");
        ui.wait_visible("#sign-out-failed");
        assert_no_offline_copies(ui);
        // The session was never ended, so the button can try again.
        assert!(ui.page.is_enabled("#sign-out").expect("enabled"));
        drop(offline);
        ui.reload_reader();
        ui.wait_text("#account-user", "reader");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_header_shows_the_catalog_label_and_opens_the_picker() {
    let (server, _) = server().await;
    server.set_catalog_label("main", "Example Notes").await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/t/main", EMAIL, PASSWORD);
        ui.wait_text("#bar-label", "Example Notes");
        assert_eq!(ui.text("#bar-mark"), "E");
        assert_eq!(
            ui.eval(
                "() => document.querySelector('#bar-search').getAttribute('href')",
                json!(null)
            ),
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
        ui.eval(
            "() => { window.scrollTo(0, 1500); return true; }",
            json!(null),
        );
        ui.poll_eval("() => window.scrollY > 0");
        ui.poll_eval("() => document.querySelector('#top').classList.contains('scrolled')");
        ui.click("#tab-home");
        ui.wait_visible("#screen-home");
        ui.poll_eval("() => window.scrollY === 0");
    })
    .await;
}

/// The breadcrumbs' root (the catalog's name until its label is known).
const CRUMB_ROOT: &str = ".crumbs [data-catalog-label]";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breadcrumbs_take_the_catalog_label_when_the_list_answers_late() {
    let (server, _) = server().await;
    server.set_catalog_label("main", "Example Notes").await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/t/main", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.wait_text(CRUMB_ROOT, "Example Notes");
        // Boot reads the catalog list before any page-side override could
        // be installed, so the answer is held in the worker (DevTools
        // pauses survive the reload) rather than with `hold_next`.
        let mut paused = ui.pause_worker_responses("*/api/v1/catalogs");
        ui.reload_reader();
        let held = paused.next_paused();
        // The tree draws without waiting for the list: its crumb names the catalog.
        ui.wait_visible("#folders");
        assert_eq!(ui.text(CRUMB_ROOT), "main");
        assert_eq!(ui.text("#bar-label"), "main");
        paused.release(&held);
        // One update, in place, once the list answers.
        ui.wait_text(CRUMB_ROOT, "Example Notes");
        ui.wait_text("#bar-label", "Example Notes");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_picker_and_the_tree_lead_from_a_catalog_to_its_concepts() {
    let (server, _) = server().await;
    server.set_catalog_label("main", "Main").await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/c", EMAIL, PASSWORD);
        // Two catalogs, so the picker stays.
        ui.wait_visible("[data-catalog=other]");
        ui.click("[data-catalog=main]");
        ui.wait_visible("#screen-home");
        ui.click("#tab-browse");
        // The root folder: an About row for its index (whose text is no
        // longer drawn above the lists), its folders, and its log on a row
        // of its own, not among the concepts (the root has none).
        ui.wait_text("#folders", "systems");
        ui.wait_visible("#about");
        assert!(!ui.present("#body"), "the index's text is behind About");
        assert_eq!(ui.text("#concepts"), "", "logs are not listed as concepts");
        ui.wait_visible("#folder-log");
        // The catalog list has been read: breadcrumbs name the catalog by its label.
        ui.wait_text("#bar-label", "Main");
        ui.click("#folders a");
        // systems/: its two concepts by title, under the catalog's label.
        ui.wait_text("#concepts", "Alpha");
        ui.wait_text("#concepts", "Beta");
        ui.wait_text("#folder-title", "systems");
        ui.wait_text(".crumbs", "Main");
        // About opens the index's text.
        ui.click("#about");
        ui.wait_text("#document-title", "About");
        ui.wait_text("#body", "Systems");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concept_shows_its_body_follows_links_and_lists_backlinks() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/v/main/systems/alpha.md", EMAIL, PASSWORD);
        ui.wait_text("#concept-title", "Alpha");
        ui.wait_text("#body", "marmalade");
        ui.wait_text(".crumbs", "systems");
        // Alpha has no sub-headings, so no outline: nothing draws in its place.
        let drawn = ui.eval(
            "() => document.querySelector('#screen-concept').textContent",
            json!(null),
        );
        assert!(
            !drawn.as_str().unwrap_or("").contains("null"),
            "stray text: {drawn}"
        );
        // render.rs's console link becomes a reader route, at the section.
        let href = ui.eval(
            "() => document.querySelector('#body a').getAttribute('href')",
            json!(null),
        );
        assert_eq!(href, json!("#/v/main/systems/beta.md?s=settings"));
        ui.click("#body a");
        ui.wait_text("#concept-title", "Beta");
        ui.wait_text("#outline", "Settings");
        // Beta is linked from alpha and from the systems index.
        ui.wait_text("#backlinks", "Alpha");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_log_opens_as_a_document() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/d/main/log.md", EMAIL, PASSWORD);
        ui.wait_text("#document-title", "Log");
        ui.wait_text("#body", "Log");
        assert!(!ui.present("#concept-gone") && !ui.present("#failed"));
    })
    .await;
}

/// Beta's body is over 20 characters: with this limit only its outline
/// comes back, with a button for the whole concept.
const SHORT_OUTLINE: usize = 20;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_whole_concept_replaces_the_outline_notice() {
    let (server, _) = server_with_outline_limit(SHORT_OUTLINE).await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/v/main/systems/beta.md", EMAIL, PASSWORD);
        ui.wait_visible("#show-full");
        assert!(!ui.present("#body"));
        ui.click("#show-full");
        ui.wait_text("#body", "Two settings");
        assert!(!ui.present("#shortened") && !ui.present("#show-full"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_concept_has_no_show_whole_button() {
    let (server, _) = server_with_outline_limit(SHORT_OUTLINE).await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/v/main/systems/empty.md", EMAIL, PASSWORD);
        ui.wait_text("#concept-title", "Empty");
        ui.wait_visible("#body");
        assert!(!ui.present("#show-full") && !ui.present("#shortened"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_whole_concept_is_not_available_offline_and_can_be_retried() {
    let (server, _) = server_with_outline_limit(SHORT_OUTLINE).await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        // Only the outline is cached: the whole body was never read.
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_visible("#show-full");
        let offline = ui.go_offline();
        ui.click("#show-full");
        ui.wait_visible("#full-failed");
        assert!(ui.page.is_enabled("#show-full").expect("enabled"));
        drop(offline);
        ui.click("#show-full");
        ui.wait_text("#body", "Two settings");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn show_the_whole_concept_after_losing_the_grant_evicts_the_catalog() {
    let (server, _) = server_with_outline_limit(SHORT_OUTLINE).await;
    let runtime = tokio::runtime::Handle::current();
    let (pool, app) = (server.db.pool.clone(), std::sync::Arc::clone(&server.app));
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_visible("#show-full");
        runtime.block_on(crate::support::server::applied(&app, async {
            ceiling::unentitle(&pool, "reader", "main", &AuditActor::cli(), Utc::now())
                .await
                .expect("unentitle");
        }));
        ui.click("#show-full");
        // The refusal went through ctx.fail: the catalog is evicted and the
        // reader goes to the picker, naming the catalog that is gone.
        ui.wait_visible("#screen-catalogs");
        assert_eq!(
            ui.eval("() => location.hash", json!(null)),
            json!("#/c?gone=main")
        );
        // The picker says so, rather than choosing the one catalog left.
        ui.wait_visible("#catalog-gone");
        let counts = ui.eval(
            "async () => (await (await caches.open('read-content')).keys()) \
             .filter(k => new URL(k.url).pathname.includes('/catalogs/main/')).length",
            json!(null),
        );
        assert_eq!(counts, json!(0));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_whole_concept_does_not_touch_a_newer_screen() {
    let (server, _) = server_with_outline_limit(SHORT_OUTLINE).await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_visible("#show-full");
        // The whole read answers late, as the worker's cached copy would
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
        ui.click("#show-full");
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

/// Signs in and opens home for `main`. The catalog picker is a later task,
/// so the catalog is chosen by opening its tree, which the reader then
/// remembers.
fn signed_in_at_home(ui: &Ui) {
    ui.sign_in_reader("/t/main", EMAIL, PASSWORD);
    ui.open_reader("/");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn home_lists_recent_changes_and_search_finds_a_concept() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        signed_in_at_home(ui);
        ui.wait_text("#recent", "Alpha");
        // Nothing but the search box is drawn in place of an absent hint.
        let drawn = ui.eval(
            "() => document.querySelector('#screen-home').textContent",
            json!(null),
        );
        assert!(
            !drawn.as_str().unwrap_or("").contains("null"),
            "stray text: {drawn}"
        );
        ui.fill("#search-input", "marmalade");
        ui.eval(
            "() => { document.querySelector('#search-form').requestSubmit(); return true; }",
            json!(null),
        );
        ui.wait_text("#hits", "Alpha");
        let drawn = ui.eval(
            "() => document.querySelector('#hits').textContent",
            json!(null),
        );
        assert!(
            !drawn.as_str().unwrap_or("").contains("null"),
            "stray text: {drawn}"
        );
        ui.click("#hits a");
        ui.wait_text("#concept-title", "Alpha");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_log_in_recent_changes_opens_as_a_document() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        signed_in_at_home(ui);
        ui.wait_visible("#recent");
        let href = ui.eval(
            "() => [...document.querySelectorAll('#recent a')].map(a => a.getAttribute('href')).find(h => h.endsWith('log.md'))",
            json!(null),
        );
        assert_eq!(href, json!("#/d/main/log.md"));
        // Several changes touch the log: follow the first of its links.
        ui.eval(
            "() => { document.querySelector(\"#recent a[href='#/d/main/log.md']\").click(); return true; }",
            json!(null),
        );
        ui.wait_text("#document-title", "Log");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn home_and_search_without_a_connection() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        // Opening search with no query makes `main` the remembered catalog
        // and reads nothing. (The tree would read the root index.md, and a
        // copy the worker kept would be listed as available offline.)
        ui.sign_in_reader("/s/main", EMAIL, PASSWORD);
        ui.wait_worker();
        assert_eq!(ui.eval(REMEMBERED_CATALOG, json!(null)), json!("main"));
        // Home was never read online, so there are no recent changes to
        // show and nothing viewed: the list says so.
        let offline = ui.go_offline();
        ui.open_reader("/");
        // Home draws the list once its read has failed: wait for it.
        ui.wait_text("#reader", "Nothing yet");
        assert!(!ui.present("#recent"));
        // Search needs the server and says so, with a way to the list.
        ui.open_reader("/s/main?q=marmalade");
        ui.wait_visible("#search-offline");
        ui.click("#offline-list-link");
        ui.wait_visible("#offline-list");
        drop(offline);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn home_lists_cached_recent_changes_and_viewed_concepts_offline() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        signed_in_at_home(ui);
        ui.wait_worker();
        // Read home and a concept online, then lose the connection: the
        // cached recent changes show with the banner, and the concept is
        // listed as readable offline.
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        ui.open_reader("/");
        ui.wait_visible("#recent");
        let offline = ui.go_offline();
        ui.reload_reader();
        ui.wait_text("#recent", "Alpha");
        ui.wait_text("#offline-banner", "Offline. Synced");
        ui.wait_text("#offline-list", "Alpha");
        drop(offline);
    })
    .await;
}

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

/// The cursor of the second page of `many`'s recent changes, as the
/// `after=` query value the page sends for "Show older".
const OLDER_CURSOR: &str = "async () => { \
    const page = await (await fetch('/api/v1/catalogs/many/recent-changes?limit=50')).json(); \
    return `&after=${encodeURIComponent(page.next_after)}`; }";

/// True in the page once `read-content` holds nothing of `many`.
const NO_MANY_CONTENT: &str = "async () => (await (await caches.open('read-content')).keys()) \
    .every(k => !new URL(k.url).pathname.includes('/catalogs/many/'))";

/// True in the page once `read-content` holds something of `many`.
const HAS_MANY_CONTENT: &str = "async () => (await (await caches.open('read-content')).keys()) \
    .some(k => new URL(k.url).pathname.includes('/catalogs/many/'))";

/// How many rows and group heads of `#recent` are drawn.
const RECENT_ROWS: &str =
    "() => document.querySelectorAll('#recent .row, #recent a.group-head').length";

/// Home of `many` (more changes than one page), signed in and with the
/// worker caching reads, "Show older" showing.
fn many_at_home(ui: &Ui) {
    ui.sign_in_reader("/t/many", EMAIL, PASSWORD);
    ui.wait_worker();
    ui.open_reader("/");
    ui.wait_visible("#recent-older");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn show_older_keeps_its_rows_and_clears_its_notice_when_a_retry_works() {
    let server = server_with_catalog("many", &notes(60, "Body.")).await;
    drive(server.base.clone(), move |ui| {
        many_at_home(ui);
        let first = ui.eval(RECENT_ROWS, json!(null));
        // Offline, the next page cannot be read: the notice shows, the rows
        // stay and the button works again.
        let offline = ui.go_offline();
        ui.click("#recent-older");
        ui.wait_visible("#older-offline");
        assert_eq!(ui.eval(RECENT_ROWS, json!(null)), first);
        ui.poll_eval("() => document.querySelector('#recent-older').disabled === false");
        // Asking again offline adds no second notice.
        ui.click("#recent-older");
        ui.poll_eval("() => document.querySelector('#recent-older').disabled === false");
        assert_eq!(
            ui.eval(
                "() => document.querySelectorAll('#older-offline').length",
                json!(null)
            ),
            json!(1)
        );
        drop(offline);
        // The connection is back: the retry reads the page, and the notice goes.
        ui.click("#recent-older");
        ui.wait_hidden("#recent-older");
        assert!(!ui.present("#older-offline"));
        let all = ui.eval(RECENT_ROWS, json!(null));
        assert!(all.as_u64() > first.as_u64(), "{first} then {all}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn show_older_with_a_lost_grant_evicts_the_catalog() {
    let (server, _) = server_with_catalog_and_reader("many", &notes(60, "Body.")).await;
    let runtime = tokio::runtime::Handle::current();
    let (pool, app) = (server.db.pool.clone(), std::sync::Arc::clone(&server.app));
    drive(server.base.clone(), move |ui| {
        many_at_home(ui);
        // The first page was cached by the worker.
        ui.poll_eval(HAS_MANY_CONTENT);
        lose_catalog(&runtime, &pool, &app, "many");
        ui.click("#recent-older");
        ui.poll_eval(NO_MANY_CONTENT);
        ui.poll_eval("() => location.hash.startsWith('#/c')");
        assert_eq!(ui.eval(REMEMBERED_CATALOG, json!(null)), json!(null));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn show_older_with_a_401_signs_out() {
    let (server, user_id) = server_with_catalog_and_reader("many", &notes(60, "Body.")).await;
    let runtime = tokio::runtime::Handle::current();
    let pool = server.db.pool.clone();
    drive(server.base.clone(), move |ui| {
        many_at_home(ui);
        revoke_sessions(&runtime, &pool, &user_id);
        ui.click("#recent-older");
        ui.wait_visible("#signin-email");
        assert_no_offline_copies(ui);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_show_older_answer_leaves_the_newer_screen_alone() {
    let server = server_with_catalog("many", &notes(60, "Body.")).await;
    drive(server.base.clone(), move |ui| {
        many_at_home(ui);
        let suffix = ui.eval(OLDER_CURSOR, json!(null));
        ui.hold_next("GET", suffix.as_str().expect("cursor"));
        ui.click("#recent-older");
        wait_held(ui);
        // Leave for the account screen, then let the page of changes arrive.
        ui.open_reader("/me");
        ui.wait_visible("#account-user");
        ui.release_held();
        assert!(ui.present("#account-user"));
        assert!(!ui.present("#recent"));
        assert!(!ui.present("#older-offline"));
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

/// Renders the real home screen into a detached section, with the
/// navigator the journey asks for (`ios`, `standalone`), and reports
/// whether the install hint is drawn.
const HOME_HINT: &str = "async ({ ios, standalone }) => { \
    const define = (name, value) => Object.defineProperty(navigator, name, { value, configurable: true }); \
    define('userAgent', ios ? 'Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X)' : 'Mozilla/5.0 (X11; Linux x86_64)'); \
    define('standalone', standalone); \
    const home = await import('/read/js/screens/home.js'); \
    const view = document.createElement('section'); \
    await home.render(view, { route: { catalog: 'main' }, state: { session: null }, live: () => true, freshness: () => {}, summary: async () => null }); \
    return view.querySelector('#install-hint') !== null; }";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_install_hint_shows_on_an_iphone_browser_until_dismissed() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_text("#account-user", "reader");
        let hint = |ios: bool, standalone: bool| {
            ui.eval(HOME_HINT, json!({ "ios": ios, "standalone": standalone }))
        };
        // Not an iPhone or iPad: no hint. Installed already: no hint.
        assert_eq!(hint(false, false), json!(false));
        assert_eq!(hint(true, true), json!(false));
        assert_eq!(hint(true, false), json!(true));
        // Dismissing it is remembered, and the hint goes.
        let dismissed = ui.eval(
            "async () => { \
             const home = await import('/read/js/screens/home.js'); \
             const view = document.createElement('section'); \
             await home.render(view, { route: { catalog: 'main' }, state: { session: null }, live: () => true, freshness: () => {}, summary: async () => null }); \
             view.querySelector('#install-hint button').click(); \
             return [view.querySelector('#install-hint') === null, localStorage.getItem('okf.read.install-hint')]; }",
            json!(null),
        );
        assert_eq!(dismissed, json!([true, "dismissed"]));
        assert_eq!(hint(true, false), json!(false));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_account_without_an_email_shows_no_null() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_text("#account-user", "reader");
        // The real account screen, for a session with no email.
        let drawn = ui.eval(
            "async () => { const me = await import('/read/js/screens/me.js'); \
             const view = document.createElement('section'); \
             await me.render(view, { state: { session: { user: 'reader', email: null, csrf: 'x', expires_at: new Date().toISOString() } } }); \
             return view.textContent; }",
            json!(null),
        );
        let drawn = drawn.as_str().unwrap_or("");
        assert!(drawn.contains("Signed in as reader"), "not drawn: {drawn}");
        assert!(!drawn.contains("null"), "stray text: {drawn}");
    })
    .await;
}

/// Tries to write 1 MB into a cache and answers the error's name.
const PROBE_WRITE: &str = "async () => { try { \
    await (await caches.open('probe')).put('/probe', new Response('y'.repeat(1e6))); return 'written'; \
    } catch (e) { return e.name; } }";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn coming_back_online_reads_fresh_content_again() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        signed_in_at_home(ui);
        ui.wait_worker();
        ui.wait_visible("#recent");
        let offline = ui.go_offline();
        ui.reload_reader();
        ui.wait_text("#offline-banner", "Offline. Synced");
        drop(offline);
        // Both the page and the worker are back on the network: a fresh
        // load shows home from the server, without the banner.
        ui.reload_reader();
        ui.wait_visible("#recent");
        ui.wait_hidden("#offline-banner");
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_text("#body", "Two settings");
        ui.wait_hidden("#offline-banner");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn viewed_concepts_are_readable_offline_and_unviewed_ones_say_so() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/t/main", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_text("#concept-title", "Beta");
        ui.open_reader("/");
        ui.wait_visible("#recent");
        let offline = ui.go_offline();
        ui.open_reader("/v/main/systems/beta.md");
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
        // The worker takes control only after the first load; the concept
        // is kept once it does and the page reads it again.
        ui.wait_worker();
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
        .map(|n| {
            (
                format!("notes/n{n:03}.md"),
                format!("---\ntitle: N{n}\n---\n# N{n}\n"),
            )
        })
        .collect();
    let mut entries: Vec<(&str, &str)> = vec![("index.md", "# Many\n")];
    entries.extend(
        notes
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str())),
    );
    let server = TestServer::start_with(vec![("many", files(&entries))], |base| Config {
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
                catalog: "many",
                access: Access::Read,
            },
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
        // Read n000 and n001, then 198 others (200 in all), then n000
        // again, which makes n001 the least recently viewed. n200 then
        // evicts n001; a cache that kept insertion order would evict n000.
        let fetched = ui.eval(
            "async () => { const get = n => fetch(`/api/v1/catalogs/many/concepts/notes/n${String(n).padStart(3, '0')}.md`); \
             await get(0); await get(1); for (let n = 2; n <= 199; n++) await get(n); await get(0); await get(200); \
             const cache = await caches.open('read-content'); const keys = (await cache.keys()).map(k => new URL(k.url).pathname); \
             return { size: keys.length, has0: keys.some(p => p.endsWith('n000.md')), has1: keys.some(p => p.endsWith('n001.md')) }; }",
            json!(null),
        );
        assert_eq!(fetched, json!({ "size": 200, "has0": true, "has1": false }));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_worker_drops_shell_caches_of_other_versions() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        // Before the worker exists: a shell cache left by an older build.
        ui.page
            .goto(
                &format!("{}/read/shell.json", ui.base),
                rustwright::GotoOptions::default(),
            )
            .expect("open the origin");
        ui.eval(
            "async () => { await caches.open('read-shell-0000000000000000'); return true; }",
            json!(null),
        );
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        let names = ui.eval("async () => await caches.keys()", json!(null));
        let names: Vec<&str> = names
            .as_array()
            .expect("names")
            .iter()
            .filter_map(|n| n.as_str())
            .collect();
        assert!(!names.contains(&"read-shell-0000000000000000"), "{names:?}");
        assert_eq!(
            names
                .iter()
                .filter(|n| n.starts_with("read-shell-"))
                .count(),
            1,
            "{names:?}"
        );
    })
    .await;
}

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
        // The override really refuses writes, or this journey proves nothing.
        assert_eq!(
            ui.eval(PROBE_WRITE, json!(null)),
            json!("QuotaExceededError")
        );
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        drop(full);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_storage_full_the_reader_boots_and_still_deletes_its_copies() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        // A quota below what is used stays exceeded when the takeover
        // deletes the old copies, so no write of any size succeeds.
        let full = ui.limit_storage_to(1.0);
        assert_eq!(
            ui.eval(PROBE_WRITE, json!(null)),
            json!("QuotaExceededError")
        );
        // "Delete offline copies" leaves no owner, so the next answer for
        // this account (any tagged one, the session included) must make it
        // the owner, which means writing the record, and under a full quota
        // it cannot.
        ui.open_reader("/me");
        ui.click("#delete-offline");
        ui.wait_visible("#offline-deleted");
        // A cacheable read still answers online, and keeps nothing.
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_text("#body", "Two settings");
        let counts = ui.eval(CACHE_COUNTS, json!(null));
        assert!(
            counts["read-content"].as_u64().unwrap_or(0) == 0
                && counts["read-owner"].as_u64().unwrap_or(0) == 0,
            "the failed takeover kept copies: {counts}"
        );
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
async fn a_failed_owner_record_write_keeps_no_copy_of_the_new_owners_reads() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        // Only the owner record's write is refused; a copy of beta could
        // be written, and must not be kept without an owner to name.
        let refused = ui.refuse_cache_puts_of("__owner");
        // "Delete offline copies" leaves no owner: the next answer for this
        // account (any tagged one, the session included) is a takeover,
        // which must write the record.
        ui.open_reader("/me");
        ui.click("#delete-offline");
        ui.wait_visible("#offline-deleted");
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_text("#body", "Two settings");
        let counts = ui.eval(CACHE_COUNTS, json!(null));
        assert!(
            counts["read-content"].as_u64().unwrap_or(0) == 0
                && counts["read-owner"].as_u64().unwrap_or(0) == 0,
            "the failed takeover kept copies: {counts}"
        );
        drop(refused);
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
        ui.open_reader("/v/main/systems/alpha.md?s=alpha");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_cache_storage_denied_the_reader_still_reads_online() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        let denied = ui.deny_cache_storage();
        assert_eq!(
            ui.eval("async () => { try { await caches.open('x'); return 'opened'; } catch (e) { return e.name; } }", json!(null)),
            json!("SecurityError")
        );
        ui.open_reader("/v/main/systems/alpha.md");
        ui.reload_reader();
        ui.wait_text("#body", "marmalade");
        // The worker was denied too: what it answered while denied is not kept.
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_text("#body", "Two settings");
        ui.open_reader("/");
        ui.wait_visible("#recent");
        ui.open_reader("/me");
        ui.click("#delete-offline");
        ui.wait_visible("#offline-deleted");
        drop(denied);
        // A fresh document, without the page-side override.
        ui.reload_reader();
        let kept = ui.eval(
            "async () => (await (await caches.open('read-content')).keys()).map(k => new URL(k.url).pathname)",
            json!(null),
        );
        // The delete above could not run while denied, so an empty cache
        // means the denied worker never kept a copy.
        assert_eq!(kept, json!([]), "the denied worker kept copies");
    })
    .await;
}

/// The account tag (`X-OKF-Account`) the server answers the page with now.
const ACCOUNT_TAG: &str =
    "async () => (await fetch('/api/v1/session')).headers.get('x-okf-account')";

/// The URL paths `read-content` holds.
const CONTENT_PATHS: &str = "async () => (await (await caches.open('read-content')).keys()) \
    .map(k => new URL(k.url).pathname)";

/// [`CONTENT_PATHS`], read in the page.
fn content_paths(ui: &Ui) -> Vec<String> {
    ui.eval(CONTENT_PATHS, json!(null))
        .as_array()
        .expect("paths")
        .iter()
        .filter_map(|path| path.as_str().map(str::to_string))
        .collect()
}

/// True in the page once `read-content` holds nothing of `main`.
const NO_MAIN_CONTENT: &str = "async () => (await (await caches.open('read-content')).keys()) \
    .every(k => !new URL(k.url).pathname.includes('/catalogs/main/'))";

/// Ends every session of the user `user_id` in the database, as an admin
/// or a password reset would.
fn revoke_sessions(runtime: &tokio::runtime::Handle, pool: &sqlx::PgPool, user_id: &str) {
    runtime.block_on(async {
        user_sessions::revoke_all(pool, user_id, None)
            .await
            .expect("revoke");
    });
}

/// Takes the reader's grant on `main` away, and waits until the server
/// applies it.
fn lose_main(
    runtime: &tokio::runtime::Handle,
    pool: &sqlx::PgPool,
    app: &std::sync::Arc<okf_mcp::app::App>,
) {
    lose_catalog(runtime, pool, app, "main");
}

/// Takes the reader's grant on `catalog` away, and waits until the server
/// applies it.
fn lose_catalog(
    runtime: &tokio::runtime::Handle,
    pool: &sqlx::PgPool,
    app: &std::sync::Arc<okf_mcp::app::App>,
    catalog: &str,
) {
    runtime.block_on(crate::support::server::applied(app, async {
        ceiling::unentitle(pool, "reader", catalog, &AuditActor::cli(), Utc::now())
            .await
            .expect("unentitle");
    }));
}

/// Imports `entries` over everything `catalog` holds and waits until the
/// server applies it, as a publish would.
async fn publish(
    pool: &sqlx::PgPool,
    app: &std::sync::Arc<okf_mcp::app::App>,
    catalog: &str,
    entries: okf_mcp::archive::Files,
) {
    let options = ImportOptions {
        replace: true,
        scope: ImportScope::Everything,
        expect_generation: None,
        expect_catalog_id: None,
        actor: "editor",
        audit: &AuditActor::cli(),
        now: Utc::now(),
    };
    crate::support::server::applied(app, async {
        import(pool, catalog, entries, &options)
            .await
            .expect("import");
    })
    .await;
}

/// Waits until the page has sent the read [`Ui::hold_next`] holds (the
/// screen that asked for it is shown, though it draws nothing yet).
fn wait_held(ui: &Ui) {
    ui.poll_eval("() => window.heldMatches >= 1");
}

/// Waits until the page has had a whole answer to a request whose URL ends
/// in `suffix` (its Resource Timing entry is recorded when it ends).
fn wait_answered(ui: &Ui, suffix: &str) {
    let answered = ui.eval(
        "(suffix) => new Promise(resolve => { const deadline = Date.now() + 10000;          const check = () => {            const done = performance.getEntriesByType('resource').some(e => e.name.endsWith(suffix));            if (done || Date.now() > deadline) resolve(done); else setTimeout(check, 50); };          check(); })",
        json!(suffix),
    );
    assert_eq!(answered, json!(true), "no answer to {suffix}");
}

/// [`main_files`] without `path`.
fn main_files_without(path: &str) -> okf_mcp::archive::Files {
    let mut entries = main_files();
    assert!(entries.remove(path).is_some(), "no {path} in main");
    entries
}

/// The account the page's owner record (`read-owner`) names, or null.
const OWNER_TAG: &str =
    "async () => { const r = await (await caches.open('read-owner')).match('/read/__owner'); \
    return r ? (await r.json()).tag : null; }";

const SECOND_EMAIL: &str = "second@example.com";

/// [`server`] with a second user, `second`, who can read `main`.
async fn server_with_a_second_reader() -> TestServer {
    let (server, _) = server().await;
    server
        .user_with_password("second", SECOND_EMAIL, PASSWORD)
        .await;
    crate::support::server::applied(&server.app, async {
        ceiling::entitle(
            &server.db.pool,
            &NewEntitlement {
                user: "second",
                catalog: "main",
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

/// Signs in as `second` in another tab (the same cookies), at the console's
/// sign-in page, so none of its answers go through the reader's worker; the
/// reader tab stays open.
fn sign_in_second_in_another_tab(ui: &Ui) {
    ui.in_second_tab(|tab| {
        tab.goto(
            &format!("{}/signin", ui.base),
            rustwright::GotoOptions::default(),
        )
        .expect("open the sign-in page");
        let options = rustwright::ActionOptions::default;
        tab.fill("#signin-email", SECOND_EMAIL, options())
            .expect("email");
        tab.fill("#signin-password", PASSWORD, options())
            .expect("password");
        tab.click("#signin-submit", options()).expect("submit");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !tab.url().contains("/ui/") {
            assert!(
                Instant::now() < deadline,
                "the second sign-in never finished"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

/// Reloads the reader on the Account screen, so boot's session check is the
/// only read, and waits until it names `second`.
fn only_the_session_check_as_second(ui: &Ui) {
    ui.open_reader("/me");
    ui.reload_reader();
    ui.wait_text("#account-user", "second");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signing_in_as_someone_else_in_another_tab_never_mixes_their_copies() {
    let server = server_with_a_second_reader().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        let first = ui.eval(OWNER_TAG, json!(null));
        assert!(first.is_string(), "{first}");
        assert!(content_paths(ui)
            .iter()
            .any(|path| path.ends_with("/alpha.md")));
        sign_in_second_in_another_tab(ui);
        // No concept is read as `second` before the copies are checked: the
        // session check alone hands them over, wiping the first account's.
        only_the_session_check_as_second(ui);
        let paths = content_paths(ui);
        assert!(
            paths.iter().all(|path| !path.ends_with("/alpha.md")),
            "{paths:?}"
        );
        let recorded = ui.eval(OWNER_TAG, json!(null));
        let second = ui.eval(ACCOUNT_TAG, json!(null));
        assert!(second.is_string(), "{second}");
        assert_ne!(first, second, "the tag names the account");
        assert_eq!(recorded, second);
        // The new owner's own reads are kept.
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_text("#concept-title", "Beta");
        let paths = content_paths(ui);
        assert!(
            paths.iter().any(|path| path.ends_with("/beta.md")),
            "{paths:?}"
        );
        assert_eq!(ui.eval(OWNER_TAG, json!(null)), second);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_check_as_someone_else_alone_hides_the_first_accounts_copies_offline() {
    let server = server_with_a_second_reader().await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/me", EMAIL, PASSWORD);
        ui.wait_worker();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_text("#body", "marmalade");
        // Recent changes too, so Home offline would draw from a kept listing.
        ui.open_reader("/");
        ui.wait_visible("#recent");
        sign_in_second_in_another_tab(ui);
        only_the_session_check_as_second(ui);
        let offline = ui.go_offline();
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_visible("#unavailable");
        ui.open_reader("/");
        let listed = ui.wait_text("#offline-list", "Nothing yet");
        assert!(!listed.contains("Alpha"), "{listed}");
        drop(offline);
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
        ui.wait_text("#account-user", "reader");
        // Boot goes on without the worker after five seconds, so the
        // account screen says nothing about the install: wait for the
        // install's read.css fetch itself, then for the install to settle.
        let deadline = Instant::now() + Duration::from_secs(10);
        while tamper.rewritten() == 0 {
            assert!(Instant::now() < deadline, "no worker fetched read.css");
            std::thread::sleep(Duration::from_millis(50));
        }
        ui.poll_eval(
            "async () => { const r = await navigator.serviceWorker.getRegistration('/read/'); \
             return !r || (!r.installing && !r.waiting); }",
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
        let shells = ui.eval(
            "async () => (await caches.keys()).filter(n => n.startsWith('read-shell-')).length",
            json!(null),
        );
        assert_eq!(shells, json!(1));
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
        assert!(!content_paths(ui).is_empty());
        revoke_sessions(&runtime, &pool, &user_id);
        ui.open_reader("/v/main/systems/alpha.md");
        ui.wait_visible("#signin-email");
        assert_no_offline_copies(ui);
        assert_eq!(ui.eval(REMEMBERED_CATALOG, json!(null)), json!(null));
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
        lose_main(&runtime, &pool, &app);
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_visible("#catalog-gone");
        let paths = content_paths(ui);
        assert!(
            paths.iter().all(|path| !path.contains("/catalogs/main/")),
            "{paths:?}"
        );
        assert!(
            paths.iter().any(|path| path.contains("/catalogs/other/")),
            "{paths:?}"
        );
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
        ui.fail_next_kind(
            "GET",
            "/concepts/systems/beta.md",
            403,
            "UnlockRequired",
            "This catalog needs an unlock.",
        );
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_visible("#locked");
        let href = ui.eval(
            "() => document.querySelector('#unlock-link')?.getAttribute('href')",
            json!(null),
        );
        assert_eq!(href, json!("/ui/"));
        // Nothing is evicted, and the catalog is still the remembered one.
        let paths = content_paths(ui);
        assert!(
            paths.iter().any(|path| path.ends_with("/alpha.md")),
            "{paths:?}"
        );
        assert_eq!(ui.eval(REMEMBERED_CATALOG, json!(null)), json!("main"));
    })
    .await;
}

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
        wait_held(ui);
        // The links read answered while the grant held: only the held read
        // can be refused.
        wait_answered(ui, "/links/systems/beta.md");
        // Leave for a screen that reads nothing, then lose the grant.
        ui.open_reader("/me");
        ui.wait_visible("#account-user");
        lose_main(&runtime, &pool, &app);
        ui.release_held();
        ui.poll_eval(NO_MAIN_CONTENT);
        // The account screen stays: only the eviction happened.
        assert!(ui.present("#account-user"));
        assert_eq!(ui.eval("() => location.hash", json!(null)), json!("#/me"));
    })
    .await;
}

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
        ui.hold_next("GET", "/catalogs/main/recent-changes?limit=50");
        ui.click("[data-catalog=main]");
        ui.wait_visible("#screen-home");
        wait_held(ui);
        // Switch to other, then lose main, then let main's answer through.
        ui.open_reader("/c");
        ui.click("[data-catalog=other]");
        ui.wait_visible("#recent");
        lose_main(&runtime, &pool, &app);
        ui.release_held();
        ui.poll_eval(NO_MAIN_CONTENT);
        let kept = ui.eval(
            "async () => ({ other: (await (await caches.open('read-content')).keys()) \
             .some(k => new URL(k.url).pathname.includes('/catalogs/other/')), \
             chosen: localStorage.getItem('okf.read.catalog') })",
            json!(null),
        );
        assert_eq!(kept, json!({ "other": true, "chosen": "other" }));
        // Home for other still shows: the late refusal drew nothing.
        assert!(ui.present("#recent"));
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
        revoke_sessions(&runtime, &pool, &user_id);
        // Beta's links read waits in the page and is never released; its
        // concept read answers 401 at once.
        ui.hold_next("GET", "/links/systems/beta.md");
        ui.open_reader("/v/main/systems/beta.md");
        ui.wait_visible("#signin-email");
        assert_no_offline_copies(ui);
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
        wait_held(ui);
        // The 404 has answered before the session ends, so the 401 is the
        // second refusal, not the first.
        wait_answered(ui, "/concepts/systems/missing.md");
        revoke_sessions(&runtime, &pool, &user_id);
        // `release_held` would wait on a flag of the page the 401 leaves.
        ui.eval("() => window.releaseHeld()", json!(null));
        ui.wait_visible("#signin-email");
        assert_no_offline_copies(ui);
    })
    .await;
}

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
        lose_main(&runtime, &pool, &app);
        ui.open_reader("/t/main");
        ui.wait_visible("#catalog-gone");
        ui.poll_eval(NO_MAIN_CONTENT);
        // Now the old 200 reaches the worker.
        paused.release(&request);
        ui.poll_eval("() => window.heldFetched === true");
        let paths = content_paths(ui);
        assert!(
            paths.iter().all(|path| !path.contains("/catalogs/main/")),
            "{paths:?}"
        );
    })
    .await;
}

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_outline_link_in_a_long_concept_keeps_the_whole_body() {
    let (server, _) = server_with_outline_limit(SHORT_OUTLINE).await;
    drive(server.base.clone(), move |ui| {
        ui.sign_in_reader("/v/main/systems/beta.md", EMAIL, PASSWORD);
        ui.click("#show-full");
        ui.wait_text("#body", "Two settings");
        ui.click("#outline summary");
        ui.mark("#body");
        ui.click("#outline a");
        // The section route draws afresh, asking for the whole body again.
        ui.poll_eval("() => location.hash.endsWith('?s=settings')");
        ui.wait_redrawn("#body");
        ui.wait_text("#body", "Two settings");
        assert!(!ui.present("#show-full") && !ui.present("#shortened"));
        ui.poll_eval(
            "() => { const heading = document.querySelector('[data-slug=\"settings\"]'); \
             return !!heading && heading.getBoundingClientRect().top < innerHeight; }",
        );
    })
    .await;
}

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
        runtime.block_on(publish(
            &pool,
            &app,
            "main",
            main_files_without("systems/alpha.md"),
        ));
        // A second read of alpha gets the 404, and the page evicts alpha.
        ui.open_reader("/me");
        ui.wait_visible("#account-user");
        ui.open_reader("/v/main/systems/alpha.md");
        let fresh = paused.next_paused();
        paused.release(&fresh);
        ui.wait_visible("#concept-gone");
        // Now the old 200 reaches the worker.
        paused.release(&old);
        ui.poll_eval("() => window.heldFetched === true");
        let paths = content_paths(ui);
        assert!(
            paths
                .iter()
                .all(|path| !path.ends_with("/concepts/systems/alpha.md")),
            "{paths:?}"
        );
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
        ui.open_reader("/d/main/systems/index.md");
        let old = paused.next_paused();
        runtime.block_on(publish(
            &pool,
            &app,
            "main",
            main_files_without("systems/index.md"),
        ));
        // A fresh read of the About page would get the 404 and evict (journey
        // `a_deleted_folder_index_drops_its_about_copy...`): make that
        // eviction directly, while the old 200 is still held.
        ui.eval(
            "async () => { await (await import('/read/js/cache.js')) \
             .evictDocument('main', 'documents', '/systems/index.md'); return true; }",
            json!(null),
        );
        paused.release(&old);
        ui.poll_eval("() => window.heldFetched === true");
        let paths = content_paths(ui);
        assert!(
            paths
                .iter()
                .all(|path| !path.ends_with("/documents/systems/index.md")),
            "{paths:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_401_at_boot_deletes_the_copies_already_on_the_device() {
    let (server, user_id) = server().await;
    let runtime = tokio::runtime::Handle::current();
    let pool = server.db.pool.clone();
    drive(server.base.clone(), move |ui| {
        signed_in_with_a_copy(ui);
        // Opening a catalog's tree remembers it.
        ui.eval("() => { location.hash = '#/t/main'; }", json!(null));
        ui.wait_visible("#screen-tree");
        assert_eq!(ui.eval(REMEMBERED_CATALOG, json!(null)), json!("main"));
        // The session ends elsewhere; the next start of the reader is
        // refused at boot, before any screen reads.
        revoke_sessions(&runtime, &pool, &user_id);
        ui.page
            .reload(rustwright::GotoOptions::default())
            .expect("reload");
        ui.wait_visible("#signin-email");
        assert!(ui.eval("() => location.pathname", json!(null)) == json!("/signin"));
        assert_no_offline_copies(ui);
        assert_eq!(ui.eval(REMEMBERED_CATALOG, json!(null)), json!(null));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_at_boot_keeps_the_copies_and_says_the_account_needs_a_connection() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        signed_in_with_a_copy(ui);
        ui.eval("() => { location.hash = '#/t/main'; }", json!(null));
        ui.wait_visible("#screen-tree");
        assert_eq!(ui.eval(REMEMBERED_CATALOG, json!(null)), json!("main"));
        let offline = ui.go_offline();
        ui.reload_reader();
        ui.eval("() => { location.hash = '#/me'; }", json!(null));
        ui.wait_visible("#account-offline");
        let counts = ui.eval(CACHE_COUNTS, json!(null));
        for name in ["read-content", "read-owner"] {
            assert!(
                counts.get(name).is_some_and(|count| count != 0),
                "{name} is gone after an offline boot: {counts}"
            );
        }
        assert!(content_paths(ui)
            .iter()
            .any(|path| path.ends_with("/alpha.md")));
        assert_eq!(ui.eval(REMEMBERED_CATALOG, json!(null)), json!("main"));
        drop(offline);
    })
    .await;
}

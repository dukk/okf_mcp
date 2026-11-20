//! The mobile reader at `/read/` (reader design): sign-in through the
//! console's session, the catalog picker, the tree, concepts and their
//! links, search, offline reading from the service worker's caches, and
//! what a revoked session, a removed grant or an unlock does to them.

use std::time::{Duration, Instant};

use chrono::Utc;
use okf_mcp::config::Config;
use okf_mcp::store::audit::AuditActor;
use okf_mcp::store::ceiling::{self, NewEntitlement};
use okf_mcp::store::keys::Access;
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
        ui.wait_text("#document-title", "log.md");
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
        ui.wait_text("#recent", "systems/alpha.md");
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
        ui.wait_text("#document-title", "log.md");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn home_and_search_without_a_connection() {
    let (server, _) = server().await;
    drive(server.base.clone(), move |ui| {
        // Opening the tree makes `main` the remembered catalog.
        ui.sign_in_reader("/t/main", EMAIL, PASSWORD);
        ui.wait_worker();
        // Home was never read online, so there are no recent changes to
        // show and nothing viewed: the list says so.
        let offline = ui.go_offline();
        ui.open_reader("/");
        ui.wait_visible("#reader");
        let shown = ui.text("#reader");
        assert!(shown.contains("Nothing yet"), "home shows: {shown}");
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
        ui.wait_text("#recent", "systems/alpha.md");
        ui.wait_text("#offline-banner", "Offline. Synced");
        ui.wait_text("#offline-list", "Alpha");
        drop(offline);
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
    await home.render(view, { route: { catalog: 'main' }, live: () => true, freshness: () => {} }); \
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
             await home.render(view, { route: { catalog: 'main' }, live: () => true, freshness: () => {} }); \
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

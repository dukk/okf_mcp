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

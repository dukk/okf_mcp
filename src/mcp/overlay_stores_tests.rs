//! The overlay cache across stores (issue #1022).
//!
//! Each store numbers its changesets from its own sequence, so the first
//! changeset on each of two stores has the same id. These tests put two
//! catalogs on two stores, give each a changeset with the same id, the same
//! catalog generation and the same version, and check that neither principal
//! ever sees the other's workspace through the shared overlay cache.

use crate::config::Config;
use crate::mcp::context::ToolContext;
use crate::mcp::documents::{get_concept, GetConceptParams};
use crate::mcp::testing::{app_with, context, open, principal_as, store_key, TestApp};
use crate::mcp::writes::{edit_document, EditDocumentParams};
use crate::store::changesets;
use crate::store::keys::Access;

const NOTE_A: &str = "---\ntype: Note\ntitle: Note\n---\nHome marker.\n";
const NOTE_B: &str = "---\ntype: Note\ntitle: Note\n---\nRemote marker.\n";

/// A context and the handle of the changeset it opened.
type Side = (ToolContext, String);

/// Two catalogs, `home` on `main` and `remote` on a store of its own, each
/// with one changeset opened by a key that reaches only its own catalog.
async fn two_stores() -> (TestApp, Side, Side) {
    let test = app_with(&[("home", &[("note.md", NOTE_A)])], Config::default()).await;
    test.catalog_on_new_store("store-b", "remote", &[("note.md", NOTE_B)])
        .await;
    store_key(&test, "key-home").await;
    store_key(&test, "key-remote").await;
    let home = context(
        &test,
        principal_as("key-home", Access::Write, &["home"], Some("home")),
        None,
    );
    let remote = context(
        &test,
        principal_as("key-remote", Access::Write, &["remote"], Some("remote")),
        None,
    );
    let home_handle = open(&home).await;
    let remote_handle = open(&remote).await;
    (test, (home, home_handle), (remote, remote_handle))
}

fn edit(handle: &str, old: &str, new: &str) -> EditDocumentParams {
    EditDocumentParams {
        path: "/note.md".to_string(),
        old_str: old.to_string(),
        new_str: new.to_string(),
        replace_all: false,
        changeset: handle.to_string(),
    }
}

async fn read(side: &Side) -> String {
    let params = GetConceptParams {
        path: "note.md".to_string(),
        catalog: None,
        links: false,
        raw: true,
        section: None,
        full: false,
        changeset: Some(side.1.clone()),
    };
    get_concept(&side.0, params)
        .await
        .expect("read through the changeset")
        .raw
        .expect("the raw text")
}

async fn change(side: &Side, old: &str, new: &str) -> Result<(), crate::error::OkfError> {
    edit_document(&side.0, edit(&side.1, old, new))
        .await
        .map(|_| ())
}

/// `(changeset id, version, catalog generation)` of the changeset `handle`
/// names in `catalog`, read from its own store.
async fn identity(test: &TestApp, catalog: &str, handle: &str) -> (i64, i64, i64) {
    let state = test.app.catalog(catalog).await.unwrap().unwrap();
    let db = test.app.store_db(state.store()).await.unwrap();
    let row = changesets::by_handle(db.executor(), handle)
        .await
        .unwrap()
        .unwrap();
    (row.id, row.version, state.current().generation())
}

#[tokio::test(flavor = "multi_thread")]
async fn colliding_changeset_ids_on_two_stores_keep_their_workspaces_apart() {
    // Arrange: one edit in each changeset, so both are at the same version.
    let (test, home, remote) = two_stores().await;
    change(&home, "Home marker", "Home draft").await.unwrap();
    change(&remote, "Remote marker", "Remote draft")
        .await
        .unwrap();
    assert_eq!(
        identity(&test, "home", &home.1).await,
        identity(&test, "remote", &remote.1).await,
        "same changeset id, version and generation on both stores"
    );

    // Act and assert: reads alternate, so each meets the other's cache entry.
    for _ in 0..2 {
        let seen = read(&home).await;
        assert!(seen.contains("Home draft"), "{seen}");
        let seen = read(&remote).await;
        assert!(seen.contains("Remote draft"), "{seen}");
        assert!(!seen.contains("Home"), "{seen}");
    }

    // An edit through `get_in` after the other catalog's overlay is cached
    // applies to its own workspace.
    read(&home).await;
    change(&remote, "Remote draft", "Remote final")
        .await
        .expect("the edit finds the remote workspace, not the home one");
    read(&remote).await;
    change(&home, "Home draft", "Home final")
        .await
        .expect("the edit finds the home workspace, not the remote one");
    assert!(read(&home).await.contains("Home final"));
    assert!(read(&remote).await.contains("Remote final"));
}

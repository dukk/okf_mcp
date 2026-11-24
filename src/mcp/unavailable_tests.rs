//! Reads across stores when one store cannot be reached (plan Task 18, P3).
//!
//! Every test puts a catalog on a store of its own (a second database) and
//! then makes that store really unreachable: its row points at a closed port
//! and its pool is dropped, so a connect is refused. Nothing here sleeps; each
//! call is bounded by a timeout.

use std::future::Future;
use std::time::Duration;

use crate::config::Config;
use crate::error::OkfError;
use crate::mcp::changesets::{list_changesets, open_changeset, ListParams, OpenParams};
use crate::mcp::documents::{get_concept, read_document, GetConceptParams, ReadDocumentParams};
use crate::mcp::history::{history, HistoryParams};
use crate::mcp::search::{run, SearchParams};
use crate::mcp::testing::{app_with, context, open, principal, writer, TestApp};
use crate::store::keys::Access;

/// Longest any call in these tests may take.
const BOUND: Duration = Duration::from_secs(30);

const HOME_NOTE: &str = "---\ntype: Note\ntitle: Home\n---\nAlpha at home.\n";
const REMOTE_NOTE: &str = "---\ntype: Note\ntitle: Remote\n---\nAlpha far away.\n";

/// An app with catalog `home` on `main` and catalog `remote` on a store of
/// its own that cannot be reached.
async fn app_with_a_lost_store() -> TestApp {
    let test = app_with(&[("home", &[("note.md", HOME_NOTE)])], Config::default()).await;
    let store = test
        .catalog_on_new_store(
            "store-b",
            "remote",
            &[("note.md", REMOTE_NOTE), ("far.md", REMOTE_NOTE)],
        )
        .await;
    test.make_unreachable(&store).await;
    test
}

/// Runs `call` under the test bound.
async fn bounded<T>(call: impl Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, call)
        .await
        .expect("the call finished within the bound, not hanging on the lost store")
}

fn search_for(query: &str, catalogs: &[&str]) -> SearchParams {
    SearchParams {
        query: query.to_string(),
        catalogs: Some(catalogs.iter().map(|name| name.to_string()).collect()),
        concept_type: None,
        tags: Vec::new(),
        status: None,
        limit: None,
        changeset: None,
    }
}

fn concept(path: &str) -> GetConceptParams {
    GetConceptParams {
        path: path.to_string(),
        catalog: None,
        links: false,
        raw: false,
        section: None,
        full: false,
        changeset: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn search_returns_other_stores_results_and_names_the_unavailable_catalog() {
    let test = app_with_a_lost_store().await;
    let ctx = context(&test, principal(&[], None), None);

    let result = bounded(run(&ctx, search_for("alpha", &["home", "remote"])))
        .await
        .unwrap();

    let found: Vec<&str> = result
        .hits
        .iter()
        .map(|hit| hit.hit.catalog.as_str())
        .collect();
    assert_eq!(found, ["home"]);
    assert_eq!(result.unavailable, ["remote"]);
    // Two lost catalogs on one store are named once each, sorted.
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["unavailable"], serde_json::json!(["remote"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_unavailable_field_is_left_out_when_every_store_answers() {
    let test = app_with_a_lost_store().await;
    let ctx = context(&test, principal(&[], None), None);

    let result = bounded(run(&ctx, search_for("alpha", &["home"])))
        .await
        .unwrap();

    assert!(result.unavailable.is_empty());
    let json = serde_json::to_value(&result).unwrap();
    assert!(json.get("unavailable").is_none(), "{json}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_catalog_the_key_does_not_reach_is_never_named_unavailable() {
    let test = app_with_a_lost_store().await;
    // The key reaches `home` only: `remote` is on the lost store.
    let ctx = context(&test, principal(&["home"], None), None);

    let named = bounded(run(&ctx, search_for("alpha", &["home", "remote"]))).await;
    let default = bounded(run(&ctx, search_for("alpha", &["home"])))
        .await
        .unwrap();

    // The refusal is the one for a catalog that does not exist.
    assert!(
        matches!(named, Err(OkfError::CatalogNotSelected { .. })),
        "{named:?}"
    );
    assert!(default.unavailable.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn list_catalogs_leaves_out_and_names_the_unavailable_catalog() {
    let test = app_with_a_lost_store().await;
    let ctx = context(&test, principal(&[], None), None);

    let listed = bounded(crate::mcp::catalogs::list(&ctx, None))
        .await
        .unwrap();

    let names: Vec<&str> = listed
        .catalogs
        .iter()
        .map(|info| info.name.as_str())
        .collect();
    assert_eq!(names, ["home"]);
    assert_eq!(listed.unavailable, ["remote"]);
    let reach_home = context(&test, principal(&["home"], None), None);
    let narrow = bounded(crate::mcp::catalogs::list(&reach_home, None))
        .await
        .unwrap();
    assert!(narrow.unavailable.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn get_concept_fails_when_an_earlier_catalogs_store_is_unavailable() {
    let test = app_with_a_lost_store().await;
    // `home` holds `note.md`, but `remote` comes first and might have held it.
    let remote_first = context(&test, principal(&[], None), Some(&["remote", "home"]));
    let home_first = context(&test, principal(&[], None), Some(&["home", "remote"]));

    let blocked = bounded(get_concept(&remote_first, concept("note"))).await;
    let blocked_history = bounded(history(
        &remote_first,
        HistoryParams {
            path: "note".to_string(),
            catalog: None,
            limit: None,
        },
    ))
    .await;
    let blocked_document = bounded(read_document(
        &remote_first,
        ReadDocumentParams {
            path: Some("/index.md".to_string()),
            catalog: None,
            changeset: None,
        },
    ))
    .await;
    // An earlier catalog that has it answers; the lost one is never asked.
    let answered = bounded(get_concept(&home_first, concept("note")))
        .await
        .unwrap();
    // A path no earlier catalog has: the lost one might, so not "not found".
    let missing = bounded(get_concept(&home_first, concept("nowhere"))).await;

    assert!(
        matches!(blocked, Err(OkfError::StoreUnavailable { .. })),
        "{blocked:?}"
    );
    assert!(matches!(
        blocked_history,
        Err(OkfError::StoreUnavailable { .. })
    ));
    assert!(matches!(
        blocked_document,
        Err(OkfError::StoreUnavailable { .. })
    ));
    assert_eq!(answered.catalog, "home");
    assert!(
        matches!(missing, Err(OkfError::StoreUnavailable { .. })),
        "{missing:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_to_an_unavailable_store_fails_with_store_unavailable() {
    let test = app_with_a_lost_store().await;
    let ctx = writer(&test, "writer", "remote").await;

    let opened = bounded(open_changeset(
        &ctx,
        OpenParams {
            catalog: Some("remote".to_string()),
        },
    ))
    .await;

    assert!(
        matches!(opened, Err(OkfError::StoreUnavailable { .. })),
        "{opened:?}"
    );
    // The message never names the store.
    assert!(!opened.unwrap_err().to_string().contains("store-b"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changeset_handle_on_a_lost_store_is_unavailable_and_others_still_resolve() {
    let test = app_with(&[("home", &[("note.md", HOME_NOTE)])], Config::default()).await;
    let ctx = writer(&test, "writer", "home").await;
    let on_main = open(&ctx).await;
    let store = test
        .catalog_on_new_store("store-b", "remote", &[("note.md", REMOTE_NOTE)])
        .await;
    let remote_ctx = context(
        &test,
        crate::mcp::testing::principal_as("writer", Access::Write, &[], Some("remote")),
        None,
    );
    let on_remote = open(&remote_ctx).await;
    test.make_unreachable(&store).await;
    let fresh = ctx.fresh();

    // `main` is asked first, so its handle resolves without touching the lost store.
    let found = bounded(fresh.changeset(&on_main, Access::Write)).await;
    // The other handle is on the lost store: a failure, not "unknown".
    let lost = bounded(ctx.fresh().changeset(&on_remote, Access::Write)).await;
    // A handle that is nowhere cannot be told from one on the lost store.
    let unknown = bounded(
        ctx.fresh()
            .changeset("00000000-0000-0000-0000-000000000000", Access::Write),
    )
    .await;
    let listed = bounded(list_changesets(&ctx.fresh(), ListParams { catalog: None }))
        .await
        .unwrap();

    assert!(found.is_ok(), "{:?}", found.err());
    assert!(
        matches!(lost, Err(OkfError::StoreUnavailable { .. })),
        "{:?}",
        lost.err()
    );
    assert!(
        matches!(unknown, Err(OkfError::StoreUnavailable { .. })),
        "{:?}",
        unknown.err()
    );
    assert_eq!(listed.unavailable, ["remote"]);
    assert_eq!(
        listed.changesets.len(),
        1,
        "main's changeset is still listed"
    );
}

//! Access accounting over REST (M14 Task 7b): an operator's content reads
//! become hourly `catalog.admin_read` entries with identities and scope
//! fixed at admission, and every export is a `catalog.exported` entry.
//! Each privacy test holds the background write (`App::access_hold`),
//! changes something, then lets the write run.

use axum::http::StatusCode;
use chrono::Utc;

use crate::db::{TEST_TEAM, TEST_USER};
use crate::http::api::testing::{app, keys, send};
use crate::mcp::testing::{settle_access, staff_root, TestApp};
use crate::store::audit::{self, AuditActor};
use crate::store::catalogs::{self, NewCatalog};
use crate::store::teams::{self, NewTeam};

/// Reads `main`'s tree as `key` with the background write held.
///
/// # Returns
/// The hold: the write runs once it is dropped.
async fn held_read(test: &TestApp, key: &str) -> tokio::sync::OwnedMutexGuard<()> {
    let hold = std::sync::Arc::clone(&test.app.access_hold)
        .lock_owned()
        .await;
    let (status, _) = send(test, "GET", "/api/v1/catalogs/main/tree", Some(key), None).await;
    assert_eq!(status, StatusCode::OK);
    hold
}

/// Every `catalog.admin_read` entry's teams, details and target name.
async fn admin_reads(test: &TestApp) -> Vec<(Vec<i64>, serde_json::Value, String)> {
    sqlx::query_as(
        "SELECT teams, details, target_name FROM audit_entries \
         WHERE action = 'catalog.admin_read' ORDER BY id",
    )
    .fetch_all(test.app.pool())
    .await
    .unwrap()
}

/// A team's `audit_teams` id.
async fn audit_team(test: &TestApp, team: &str) -> i64 {
    let mut conn = test.app.pool().acquire().await.unwrap();
    audit::team_ref(&mut conn, team).await.unwrap()
}

/// A catalog's `catalogs.id`.
async fn catalog_id(test: &TestApp, name: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM catalogs WHERE name = $1")
        .bind(name)
        .fetch_one(test.app.pool())
        .await
        .unwrap()
}

/// Creates team `family`, owned by the test user.
async fn family(test: &TestApp) {
    let team = NewTeam {
        name: "family",
        display_name: "",
        owner: TEST_USER,
    };
    teams::create(test.app.pool(), &team, &AuditActor::cli(), Utc::now())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_operator_read_is_bucketed_and_a_members_read_is_not() {
    let test = app().await;
    let scoped = keys(&test).await;
    let staff = staff_root(&test).await;

    send(
        &test,
        "GET",
        "/api/v1/catalogs/main/tree",
        Some(&staff),
        None,
    )
    .await;
    let document = "/api/v1/catalogs/main/documents/systems/index.md";
    send(&test, "GET", document, Some(&staff), None).await;
    // A member's read (the test user owns main), and an admin key of the
    // member: neither is an admin read.
    send(
        &test,
        "GET",
        "/api/v1/catalogs/main/tree",
        Some(&scoped.read),
        None,
    )
    .await;
    send(
        &test,
        "GET",
        "/api/v1/catalogs/main/tree",
        Some(&scoped.admin),
        None,
    )
    .await;
    settle_access(&test).await;

    let entries = audit::testing::entries(test.app.pool(), "catalog.admin_read").await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].requests, Some(2), "one request, one count");
    assert_eq!(entries[0].target.as_deref(), Some("main"));
    assert_eq!(entries[0].actor_person.as_deref(), Some("staff"));
    assert_eq!(entries[0].actor_label, "api key");
    assert!(entries[0].actor_operator);
    assert_eq!(entries[0].teams, [TEST_TEAM]);
    assert_eq!(entries[0].source, "api");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_delayed_past_the_users_deletion_stores_nothing_that_identifies_them() {
    let test = app().await;
    let staff = staff_root(&test).await;
    let hold = held_read(&test, &staff).await;

    crate::store::users::delete(
        test.app.pool(),
        "staff",
        true,
        &AuditActor::cli(),
        Utc::now(),
    )
    .await
    .unwrap();
    drop(hold);
    settle_access(&test).await;

    // Every audit row, as text.
    let stored: String = sqlx::query_scalar(
        "SELECT coalesce((SELECT string_agg(row_to_json(e)::text, '') FROM audit_entries e), '') \
             || coalesce((SELECT string_agg(row_to_json(c)::text, '') \
                          FROM audit_credentials c), '') \
             || coalesce((SELECT string_agg(row_to_json(p)::text, '') FROM audit_people p), '') \
             || coalesce((SELECT string_agg(bucket, '') FROM audit_access_counts), '')",
    )
    .fetch_one(test.app.pool())
    .await
    .unwrap();
    assert_eq!(
        admin_reads(&test).await.len(),
        1,
        "the read is still recorded"
    );
    assert!(
        !stored.contains("staff") && !stored.contains("Staff"),
        "{stored}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_admitted_before_a_transfer_stays_with_the_old_owner() {
    let test = app().await;
    let staff = staff_root(&test).await;
    family(&test).await;
    let personal = audit_team(&test, TEST_TEAM).await;
    let hold = held_read(&test, &staff).await;

    catalogs::set_owner(
        test.app.pool(),
        "main",
        "family",
        &AuditActor::cli(),
        Utc::now(),
    )
    .await
    .unwrap();
    drop(hold);
    settle_access(&test).await;

    let reads = admin_reads(&test).await;
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].0, [personal]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_of_a_catalog_deleted_or_replaced_before_it_is_written_keeps_its_identity() {
    let test = app().await;
    let staff = staff_root(&test).await;
    family(&test).await;
    let personal = audit_team(&test, TEST_TEAM).await;
    let original = catalog_id(&test, "main").await;
    let hold = held_read(&test, &staff).await;

    // Deleted, then a new `main` for another team, before the write runs.
    catalogs::delete(
        test.app.pool(),
        "main",
        true,
        &AuditActor::cli(),
        Utc::now(),
    )
    .await
    .unwrap();
    let replacement = NewCatalog {
        name: "main",
        description: "",
        owner_team: "family",
    };
    catalogs::create(
        test.app.pool(),
        &replacement,
        &AuditActor::cli(),
        Utc::now(),
    )
    .await
    .unwrap();
    drop(hold);
    settle_access(&test).await;
    test.app.poll().await.unwrap();
    // A read of the replacement is another entry, in its own scope.
    let (status, _) = send(
        &test,
        "GET",
        "/api/v1/catalogs/main/tree",
        Some(&staff),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    settle_access(&test).await;

    let reads = admin_reads(&test).await;
    let family = audit_team(&test, "family").await;
    let replacement = catalog_id(&test, "main").await;
    assert_eq!(reads.len(), 2, "neither dropped nor merged");
    assert_eq!(
        (reads[0].0.clone(), reads[0].1["catalog_id"].as_i64()),
        (vec![personal], Some(original))
    );
    assert_eq!(
        (reads[1].0.clone(), reads[1].1["catalog_id"].as_i64()),
        (vec![family], Some(replacement))
    );
    assert_eq!(reads[0].2, "main", "the name as it was");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_export_is_recorded_whoever_asks() {
    let test = app().await;
    let scoped = keys(&test).await;
    let staff = staff_root(&test).await;

    let (member, _) = send(
        &test,
        "GET",
        "/api/v1/catalogs/main/export",
        Some(&scoped.read),
        None,
    )
    .await;
    let (operator, _) = send(
        &test,
        "GET",
        "/api/v1/catalogs/main/export",
        Some(&staff),
        None,
    )
    .await;

    assert_eq!((member, operator), (StatusCode::OK, StatusCode::OK));
    let entries = audit::testing::entries(test.app.pool(), "catalog.exported").await;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].source, "api");
    assert_eq!(entries[0].actor_person.as_deref(), Some(TEST_USER));
    assert!(!entries[0].actor_operator);
    assert_eq!(entries[1].actor_person.as_deref(), Some("staff"));
    assert!(entries[1].actor_operator);
    assert_eq!(entries[0].teams, [TEST_TEAM]);
    assert!(entries[0].details["files"].as_i64().unwrap() > 0);
    // An export is its own entry, not an admin read.
    settle_access(&test).await;
    assert!(
        audit::testing::entries(test.app.pool(), "catalog.admin_read")
            .await
            .is_empty()
    );
}

/// Deletes `main` and creates it again for a new team `acme` of a new user
/// `bob` (so the test user is not a member of the replacement).
async fn replace_main_for_acme(test: &TestApp) {
    let pool = test.app.pool();
    let cli = AuditActor::cli();
    let bob = crate::store::users::NewUser {
        name: "bob",
        display_name: "",
    };
    crate::store::users::create(pool, &bob, &cli, Utc::now())
        .await
        .unwrap();
    let acme = NewTeam {
        name: "acme",
        display_name: "",
        owner: "bob",
    };
    teams::create(pool, &acme, &cli, Utc::now()).await.unwrap();
    catalogs::delete(pool, "main", true, &cli, Utc::now())
        .await
        .unwrap();
    let main = NewCatalog {
        name: "main",
        description: "",
        owner_team: "acme",
    };
    catalogs::create(pool, &main, &cli, Utc::now())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_of_an_old_catalog_map_under_newer_gates_is_refused_not_misrecorded() {
    let test = app().await;
    let staff = staff_root(&test).await;
    let original = catalog_id(&test, "main").await;
    replace_main_for_acme(&test).await;
    // What `App::poll` does first: the key cache (and its gates) moves to
    // the replacement while the catalog map still holds the old `main`.
    test.app.keys().reload(test.app.pool()).await.unwrap();
    let ctx = crate::mcp::testing::context(
        &test,
        crate::mcp::testing::cached_principal(&test, &staff),
        None,
    );
    assert_eq!(ctx.catalogs()["main"].id(), original, "the old map");

    let state = ctx.one(Some("main")).unwrap();
    let refused = ctx.admit(&state, audit::AccessKind::Read);
    settle_access(&test).await;

    assert!(
        matches!(refused, Err(crate::error::OkfError::Busy { .. })),
        "{refused:?}"
    );
    assert!(admin_reads(&test).await.is_empty(), "nothing misrecorded");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_old_principal_reading_a_new_catalog_map_is_admitted_as_the_new_catalog() {
    let test = app().await;
    // The test user's every-catalog admin key: a member of the old `main`'s
    // owner team, not of the replacement's.
    let root = crate::http::api::testing::every_catalog_admin(&test).await;
    let old_principal = crate::mcp::testing::cached_principal(&test, &root);
    replace_main_for_acme(&test).await;
    test.app.poll().await.unwrap();
    let ctx = crate::mcp::testing::context(&test, old_principal, None);

    let state = ctx.one(Some("main")).unwrap();
    ctx.admit(&state, audit::AccessKind::Read).unwrap();
    settle_access(&test).await;

    let reads = admin_reads(&test).await;
    let acme = audit_team(&test, "acme").await;
    let replacement = catalog_id(&test, "main").await;
    assert_eq!(
        reads.len(),
        1,
        "an admin read of the replacement, not a member's"
    );
    assert_eq!(
        (reads[0].0.clone(), reads[0].1["catalog_id"].as_i64()),
        (vec![acme], Some(replacement))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_export_whose_entry_cannot_be_written_answers_500_without_an_archive() {
    let test = app().await;
    let scoped = keys(&test).await;
    crate::http::api::testing::fail_on(
        test.app.pool(),
        "BEFORE INSERT ON audit_entries FOR EACH ROW",
    )
    .await;

    let (status, body) = send(
        &test,
        "GET",
        "/api/v1/catalogs/main/export",
        Some(&scoped.read),
        None,
    )
    .await;

    crate::http::api::testing::allow(test.app.pool(), "audit_entries").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"], "Database", "an error body, not an archive");
}

/// A tool context for `secret` on a connection of `main` then `other`.
fn on_main_and_other(test: &TestApp, secret: &str) -> crate::mcp::context::ToolContext {
    crate::mcp::testing::context(
        test,
        crate::mcp::testing::cached_principal(test, secret),
        Some(&["main", "other"]),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_that_answers_from_the_first_catalog_records_only_that_one() {
    use crate::mcp::documents::{get_concept, GetConceptParams};
    use crate::mcp::history::{history, HistoryParams};
    let test = app().await;
    let staff = staff_root(&test).await;
    // `main`, listed first, answers each: both catalogs have `index.md`'s
    // history, and only `main` has the concept.
    let history_ctx = on_main_and_other(&test, &staff);
    let concept_ctx = on_main_and_other(&test, &staff);

    let found = history(
        &history_ctx,
        HistoryParams {
            path: "index.md".to_string(),
            catalog: None,
            limit: None,
        },
    )
    .await
    .unwrap();
    let concept = get_concept(
        &concept_ctx,
        GetConceptParams {
            path: "systems/alpha.md".to_string(),
            catalog: None,
            links: false,
            raw: false,
            section: None,
            full: false,
            changeset: None,
        },
    )
    .await;
    settle_access(&test).await;

    assert_eq!(found.catalog, "main");
    assert!(concept.is_ok(), "{concept:?}");
    let entries = audit::testing::entries(test.app.pool(), "catalog.admin_read").await;
    assert_eq!(
        entries.len(),
        1,
        "never `other`, which was not read: {entries:?}"
    );
    assert_eq!(entries[0].target.as_deref(), Some("main"));
    assert_eq!(entries[0].requests, Some(2), "one per call");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_search_refused_after_one_catalog_passes_queues_no_read() {
    use crate::mcp::search::{run, SearchParams};
    let test = app().await;
    let staff = staff_root(&test).await;
    // `other` is replaced and only the key cache reloaded: `main` passes
    // its check, `other` (still the old map's) cannot.
    let cli = AuditActor::cli();
    catalogs::delete(test.app.pool(), "other", true, &cli, Utc::now())
        .await
        .unwrap();
    let other = NewCatalog {
        name: "other",
        description: "",
        owner_team: TEST_TEAM,
    };
    catalogs::create(test.app.pool(), &other, &cli, Utc::now())
        .await
        .unwrap();
    test.app.keys().reload(test.app.pool()).await.unwrap();
    let ctx = on_main_and_other(&test, &staff);

    let refused = run(
        &ctx,
        SearchParams {
            query: "Main".to_string(),
            catalogs: None,
            concept_type: None,
            tags: Vec::new(),
            status: None,
            limit: None,
            changeset: None,
        },
    )
    .await
    .map(drop);
    settle_access(&test).await;

    assert!(
        matches!(refused, Err(crate::error::OkfError::Busy { .. })),
        "{refused:?}"
    );
    assert!(
        admin_reads(&test).await.is_empty(),
        "nothing queued for `main`"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_maintenance_mutation_records_no_read_and_a_revision_read_records_one() {
    use crate::mcp::maintenance::work::{
        maintenance_claim, maintenance_read_revision, MaintenanceClaimParams,
        MaintenanceReadRevisionParams,
    };
    let test = app().await;
    // An admin key with reporting off: its claim is refused, after the
    // target catalog is resolved.
    let staff = staff_root(&test).await;
    let claim_ctx = crate::mcp::testing::context(
        &test,
        crate::mcp::testing::cached_principal(&test, &staff),
        None,
    );
    let read_ctx = crate::mcp::testing::context(
        &test,
        crate::mcp::testing::cached_principal(&test, &staff),
        None,
    );

    let claimed = maintenance_claim(
        &claim_ctx,
        MaintenanceClaimParams {
            catalog: "main".to_string(),
            idempotency_key: "claim-1".to_string(),
            batch_size: None,
        },
    )
    .await
    .map(drop);
    settle_access(&test).await;
    let after_claim = admin_reads(&test).await.len();
    let read = maintenance_read_revision(
        &read_ctx,
        MaintenanceReadRevisionParams {
            catalog: "main".to_string(),
            path: "systems/alpha.md".to_string(),
            revision: 1,
        },
    )
    .await
    .map(drop);
    settle_access(&test).await;

    assert!(
        matches!(claimed, Err(crate::error::OkfError::Forbidden { .. })),
        "{claimed:?}"
    );
    assert_eq!(after_claim, 0, "a refused claim reads nothing");
    assert!(read.is_ok(), "{read:?}");
    assert_eq!(
        admin_reads(&test).await.len(),
        1,
        "the document text was read"
    );
}

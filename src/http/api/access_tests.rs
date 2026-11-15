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

//! Tests for the directory's queries: [`row_by_name`], [`rows_by_names`]
//! and [`page`] (M18 Task 5).

use chrono::{DateTime, TimeZone, Utc};
use sqlx::PgPool;

use super::*;
use crate::db::{test_db, TEST_TEAM};
use crate::labels::DisplayName;
use crate::store::audit::AuditActor;
use crate::store::teams::{self, NewTeam};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap()
}

/// Creates catalog `name`, owned by `owner`.
async fn seed(pool: &PgPool, name: &str, owner: &str) {
    let new = NewCatalog {
        name,
        display_name: &DisplayName::default(),
        description: "about it",
        owner_team: owner,
    };
    create(pool, &new, &AuditActor::cli(), now()).await.unwrap();
}

/// Creates the shared team `name`, owned by the fixture user.
async fn team(pool: &PgPool, name: &str) {
    let new = NewTeam {
        name,
        display_name: &DisplayName::default(),
        owner: TEST_TEAM,
    };
    teams::create(pool, &new, &AuditActor::cli(), now())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_row_carries_the_owner_and_its_access_facts() {
    let db = test_db().await;
    team(&db.pool, "ops").await;
    seed(&db.pool, "plain", TEST_TEAM).await;
    seed(&db.pool, "held", "ops").await;
    seed(&db.pool, "quiet", TEST_TEAM).await;
    sqlx::query("INSERT INTO catalogs (name, created_at) VALUES ('legacy', now())")
        .execute(&db.pool)
        .await
        .unwrap();
    set_maintenance(
        &db.pool,
        &MaintenanceChange {
            catalog: "quiet",
            to: false.into(),
            owner: None,
        },
        &AuditActor::cli(),
        now(),
    )
    .await
    .unwrap();
    teams::suspend(&db.pool, "ops", Some("billing"), &AuditActor::cli(), now())
        .await
        .unwrap();

    let plain = row_by_name(&db.pool, "plain").await.unwrap().unwrap();
    let held = row_by_name(&db.pool, "held").await.unwrap().unwrap();
    let quiet = row_by_name(&db.pool, "quiet").await.unwrap().unwrap();
    let legacy = row_by_name(&db.pool, "legacy").await.unwrap().unwrap();

    // The values the key cache holds today (`teams::suspended_catalogs`,
    // `gates::load`), read once.
    let found = find(&db.pool, "plain").await.unwrap();
    assert_eq!(plain.id, found.id);
    assert_eq!(plain.generation, found.generation);
    assert_eq!(plain.description, "about it");
    assert_eq!(plain.owner_team.as_deref(), Some(TEST_TEAM));
    assert_eq!(plain.owner_team_id.as_deref(), Some(TEST_TEAM));
    assert_eq!(plain.suspended, None);
    assert!(!plain.maintenance_off);
    let audit_team: Option<i64> =
        sqlx::query_scalar("SELECT id FROM audit_teams WHERE team_id = $1")
            .bind(TEST_TEAM)
            .fetch_optional(&db.pool)
            .await
            .unwrap();
    assert!(audit_team.is_some());
    assert_eq!(plain.audit_team, audit_team);

    assert_eq!(
        held.suspended,
        Some(SuspendedRow {
            team: "ops".to_owned(),
            notice: Some("billing".to_owned()),
        })
    );
    let suspended = teams::suspended_catalogs(&db.pool).await.unwrap();
    assert_eq!(suspended.len(), 1);
    assert_eq!(suspended["held"].team, "ops");
    assert!(quiet.maintenance_off);
    assert!(!held.maintenance_off);

    assert_eq!(legacy.owner_team, None);
    assert_eq!(legacy.owner_team_id, None);
    assert_eq!(legacy.suspended, None);
    assert_eq!(legacy.audit_team, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_or_malformed_name_has_no_row() {
    let db = test_db().await;
    seed(&db.pool, "pg", TEST_TEAM).await;

    assert_eq!(row_by_name(&db.pool, "nope").await.unwrap(), None);
    // A NUL would be a database error if it were bound.
    assert_eq!(row_by_name(&db.pool, "p\0g").await.unwrap(), None);
    assert_eq!(row_by_name(&db.pool, "").await.unwrap(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn rows_by_names_reads_the_known_ones_in_name_order() {
    let db = test_db().await;
    for name in ["c", "a", "b"] {
        seed(&db.pool, name, TEST_TEAM).await;
    }
    let ask = |names: &[&str]| {
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>()
    };

    let rows = rows_by_names(&db.pool, &ask(&["c", "zzz", "a", "b\0", "a"]))
        .await
        .unwrap();

    let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, ["a", "c"]);
    assert!(rows_by_names(&db.pool, &[]).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn page_walks_three_pages_of_five_in_name_order() {
    let db = test_db().await;
    for index in (0..15).rev() {
        seed(&db.pool, &format!("c{index:02}"), TEST_TEAM).await;
    }

    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    let mut pages = 0;
    loop {
        let rows = page(&db.pool, after.as_deref(), 5).await.unwrap();
        if rows.is_empty() {
            break;
        }
        pages += 1;
        assert_eq!(rows.len(), 5);
        after = rows.last().map(|row| row.name.clone());
        seen.extend(rows.into_iter().map(|row| row.name));
    }

    assert_eq!(pages, 3);
    let expected: Vec<String> = (0..15).map(|index| format!("c{index:02}")).collect();
    assert_eq!(seen, expected);
    // A cursor that cannot name a catalog ends the list rather than failing.
    assert!(page(&db.pool, Some("c\0"), 5).await.unwrap().is_empty());
}

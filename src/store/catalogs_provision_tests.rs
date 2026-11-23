//! Tests for creating a catalog in two steps and for recovering one that
//! stopped half-way (plan Task 10b, decision P9).

use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::Notify;

use super::*;
use crate::db::{test_db, TEST_TEAM, TEST_USER};
use crate::labels::DisplayName;
use crate::store::audit::testing::entries;
use crate::store::audit::AuditActor;
use crate::store::ceiling;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 23, 12, 0, 0).unwrap()
}

/// The display name every test catalog has: none.
static NO_DISPLAY_NAME: std::sync::LazyLock<DisplayName> =
    std::sync::LazyLock::new(DisplayName::default);

fn new_catalog(name: &str) -> NewCatalog<'_> {
    NewCatalog {
        name,
        display_name: &NO_DISPLAY_NAME,
        description: "about it",
        owner_team: TEST_TEAM,
    }
}

/// Step 1 only: the registry row exists, unprovisioned, with no store side.
async fn half_made(pool: &PgPool, name: &str) -> (Catalog, Pending) {
    let (catalog, store) = register(pool, &new_catalog(name), &AuditActor::cli(), now())
        .await
        .unwrap();
    let pending = Pending {
        id: catalog.id,
        name: catalog.name.clone(),
        store: store.to_string(),
    };
    (catalog, pending)
}

async fn document_count(pool: &PgPool, id: i64) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM documents WHERE catalog_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn row_exists(pool: &PgPool, id: i64) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM catalogs WHERE id = $1)")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Waits until `task` ends or Postgres reports a session waiting for any
/// lock: the condition the concurrency tests need, with no sleep. Returns
/// whether the waiter waits for an advisory lock, the catalog's lock (a
/// session blocked on anything else means the lock did not do its job).
async fn wait_for_waiter_or_end<T>(pool: &PgPool, task: &tokio::task::JoinHandle<T>) -> bool {
    loop {
        if task.is_finished() {
            return false;
        }
        let waiting: Option<String> = sqlx::query_scalar(
            // This test's database only: the server is shared.
            "SELECT l.locktype FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
                 WHERE NOT l.granted AND a.datname = current_database() LIMIT 1",
        )
        .fetch_optional(pool)
        .await
        .unwrap();
        if let Some(locktype) = waiting {
            return locktype == "advisory";
        }
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_half_created_catalog_is_not_visible() {
    let db = test_db().await;
    let (catalog, _) = half_made(&db.pool, "half").await;
    let actor = AuditActor::cli();

    assert!(matches!(
        find(&db.pool, "half").await,
        Err(OkfError::CatalogNotSelected { .. })
    ));
    assert!(list(&db.pool).await.unwrap().is_empty());
    assert_eq!(row_by_name(&db.pool, "half").await.unwrap(), None);
    assert_eq!(row_by_id(&db.pool, catalog.id).await.unwrap(), None);
    // Nothing that finds a catalog by name can act on it either.
    assert!(matches!(
        delete(&db.pool, "half", true, &actor, now()).await,
        Err(OkfError::CatalogNotSelected { .. })
    ));
    assert!(matches!(
        set_owner(&db.pool, "half", TEST_TEAM, &actor, now()).await,
        Err(OkfError::CatalogNotSelected { .. })
    ));
    let mut conn = db.pool.acquire().await.unwrap();
    assert!(matches!(
        lock_by_name(&mut conn, "half").await,
        Err(OkfError::CatalogNotSelected { .. })
    ));
    // The name is taken, though: a second create is refused.
    drop(conn);
    let again = create(&db.pool, &new_catalog("half"), &actor, now()).await;
    assert!(matches!(again, Err(OkfError::Refused { .. })), "{again:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_half_created_catalog_is_outside_every_ceiling() {
    let db = test_db().await;
    let (catalog, _) = half_made(&db.pool, "half").await;
    let user_id: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(TEST_USER)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    // An entitlement row that should not exist (nothing can name the
    // catalog to grant one): the readers still leave it out.
    sqlx::query(
        "INSERT INTO user_catalogs (user_id, catalog_id, access, created_at) \
         VALUES ($1, $2, 'write', now())",
    )
    .bind(&user_id)
    .bind(catalog.id)
    .execute(&db.pool)
    .await
    .unwrap();

    let mut conn = db.pool.acquire().await.unwrap();
    let named = ceiling::ceiling_in(&mut conn, &user_id, &["half".to_string()])
        .await
        .unwrap();
    assert!(named.is_empty(), "{named:?}");
    let entitled = ceiling::entitlements_in(&mut conn, TEST_USER)
        .await
        .unwrap();
    assert!(entitled.is_empty());
    drop(conn);
    assert_eq!(
        ceiling::highest(&db.pool, &user_id).await.unwrap(),
        crate::store::keys::Access::Read,
        "the owner team's write on a half-made catalog is no ceiling"
    );
    let page = ceiling::page(&db.pool, &user_id, "", None, 10)
        .await
        .unwrap();
    assert!(page.entries.is_empty(), "{:?}", page.entries);
    let refused = ceiling::set_member_access(
        &db.pool,
        "half",
        MemberAccess::Read,
        &AuditActor::cli(),
        now(),
    )
    .await;
    assert!(matches!(refused, Err(OkfError::CatalogNotSelected { .. })));
    // The console's reads and the entitlement writes by name.
    assert!(matches!(
        ceiling::member_access(&db.pool, "half").await,
        Err(OkfError::CatalogNotSelected { .. })
    ));
    let page = ceiling::TeamPage {
        members_after: "",
        entitlements_after: "",
        limit: 10,
        lists: ceiling::TeamLists::Both,
    };
    assert!(ceiling::team_access(&db.pool, "half", None, &page)
        .await
        .is_err());
    let entitle = ceiling::NewEntitlement {
        user: TEST_USER,
        catalog: "half",
        access: crate::store::keys::Access::Read,
    };
    assert!(
        ceiling::entitle(&db.pool, &entitle, &AuditActor::cli(), now())
            .await
            .is_err()
    );
    // The details and maintenance changes by name.
    let details = DetailsChange {
        catalog: "half",
        display_name: None,
        description: Some("x"),
        owner: None,
    };
    assert!(matches!(
        update_details(&db.pool, &details, &AuditActor::cli(), now()).await,
        Err(OkfError::CatalogNotSelected { .. })
    ));
    let maintenance = MaintenanceChange {
        catalog: "half",
        to: true.into(),
        owner: None,
    };
    assert!(matches!(
        set_maintenance(&db.pool, &maintenance, &AuditActor::cli(), now()).await,
        Err(OkfError::CatalogNotSelected { .. })
    ));

    // Once provisioned, the same readers show it.
    mark_provisioned(&db.pool, catalog.id, now()).await.unwrap();
    let page = ceiling::page(&db.pool, &user_id, "", None, 10)
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_half_created_catalog_is_not_listed_granted_or_repaired() {
    let db = test_db().await;
    let (catalog, _) = half_made(&db.pool, "half").await;

    let team = crate::store::teams::find(&db.pool, TEST_TEAM)
        .await
        .unwrap();
    assert!(team.catalogs.is_empty(), "{:?}", team.catalogs);
    let mut conn = db.pool.acquire().await.unwrap();
    let granted = crate::store::keys::catalog_ids(&mut conn, &["half".to_string()]).await;
    assert!(matches!(granted, Err(OkfError::CatalogNotSelected { .. })));
    drop(conn);
    // The repair would otherwise find no root index and write one, which
    // is the store side the recovery task checks for.
    let missing = crate::store::repair::missing_root_index(
        &crate::stores::AccountDb::of(&db.pool),
        &crate::stores::CatalogDb::main_of(&db.pool),
    )
    .await
    .unwrap();
    assert!(missing.is_empty(), "{missing:?}");

    mark_provisioned(&db.pool, catalog.id, now()).await.unwrap();
    let team = crate::store::teams::find(&db.pool, TEST_TEAM)
        .await
        .unwrap();
    assert_eq!(team.catalogs, ["half"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn create_leaves_a_whole_catalog_and_announces_it_once() {
    let db = test_db().await;
    let made = create(&db.pool, &new_catalog("whole"), &AuditActor::cli(), now())
        .await
        .unwrap();

    assert_eq!(find(&db.pool, "whole").await.unwrap(), made);
    assert_eq!(document_count(&db.pool, made.id).await, 2);
    let created = entries(&db.pool, "catalog.created").await;
    assert_eq!(created.len(), 1, "{created:?}");
    let provisioned: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT provisioned_at FROM catalogs WHERE id = $1")
            .bind(made.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(provisioned, Some(now()));
}

#[tokio::test(flavor = "multi_thread")]
async fn provisioning_job_finishes_a_catalog_whose_store_side_exists() {
    let db = test_db().await;
    let (catalog, pending) = half_made(&db.pool, "stopped").await;
    // The create got as far as committing its store side.
    let store = CatalogDb::main_of(&db.pool);
    provision_store_side(&store, &catalog, "cli", now(), std::future::ready(()))
        .await
        .unwrap();

    let outcome = recover(&store, &pending, now()).await.unwrap();

    assert_eq!(outcome, Recovery::Finished);
    assert_eq!(find(&db.pool, "stopped").await.unwrap().id, catalog.id);
    assert_eq!(document_count(&db.pool, catalog.id).await, 2);
    assert!(entries(&db.pool, "catalog.deleted").await.is_empty());
    // Nothing is left to recover, and a second look changes nothing.
    assert!(pending_rows(&db.pool).await.is_empty());
    assert_eq!(
        recover(&store, &pending, now()).await.unwrap(),
        Recovery::Settled
    );
}

async fn pending_rows(pool: &PgPool) -> Vec<Pending> {
    pending(pool, now() + chrono::TimeDelta::days(1))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn provisioning_job_removes_a_catalog_whose_store_side_never_happened() {
    let db = test_db().await;
    let (catalog, pending) = half_made(&db.pool, "lost").await;
    let store = CatalogDb::main_of(&db.pool);

    let outcome = recover(&store, &pending, now()).await.unwrap();

    assert_eq!(outcome, Recovery::Removed);
    assert!(!row_exists(&db.pool, catalog.id).await);
    assert_eq!(document_count(&db.pool, catalog.id).await, 0);
    // The failed create is in the audit details, for the owner team.
    let deleted = entries(&db.pool, "catalog.deleted").await;
    assert_eq!(deleted.len(), 1, "{deleted:?}");
    assert_eq!(deleted[0].target.as_deref(), Some("lost"));
    assert_eq!(deleted[0].teams, [TEST_USER]);
    assert_eq!(
        deleted[0].details,
        json!({"forced": false, "generation": 0, "failed_create": true})
    );
    // The name is free again.
    create(&db.pool, &new_catalog("lost"), &AuditActor::cli(), now())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn pending_lists_only_old_unprovisioned_rows_oldest_first() {
    let db = test_db().await;
    let early = now() - chrono::TimeDelta::minutes(5);
    for (name, at) in [
        ("second", now() - chrono::TimeDelta::minutes(2)),
        ("first", early),
    ] {
        register(&db.pool, &new_catalog(name), &AuditActor::cli(), at)
            .await
            .unwrap();
    }
    create(&db.pool, &new_catalog("whole"), &AuditActor::cli(), early)
        .await
        .unwrap();
    register(&db.pool, &new_catalog("young"), &AuditActor::cli(), now())
        .await
        .unwrap();

    let found = pending(&db.pool, now() - chrono::TimeDelta::minutes(1))
        .await
        .unwrap();

    let names: Vec<&str> = found.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, ["first", "second"]);
    assert_eq!(found[0].store, "main");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_stalled_past_the_interval_is_not_removed_while_it_holds_the_lock() {
    let db = test_db().await;
    let (catalog, pending) = half_made(&db.pool, "slow").await;
    let store = CatalogDb::main_of(&db.pool);
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));

    // The create has the lock and its files written, and stalls before commit.
    let create_task = {
        let (store, catalog) = (store.clone(), catalog.clone());
        let (entered, release) = (entered.clone(), release.clone());
        tokio::spawn(async move {
            provision_store_side(&store, &catalog, "cli", now(), async move {
                entered.notify_one();
                release.notified().await;
            })
            .await
        })
    };
    entered.notified().await;
    // Recovery looks at it now, as the housekeeping task would after the interval.
    let recovery_task = {
        let (store, pending) = (store.clone(), pending.clone());
        tokio::spawn(async move { recover(&store, &pending, now()).await })
    };
    let waited = wait_for_waiter_or_end(&db.pool, &recovery_task).await;
    release.notify_one();

    create_task.await.unwrap().unwrap();
    let outcome = recovery_task.await.unwrap().unwrap();
    assert!(waited, "recovery should wait for the create's lock");
    assert_eq!(outcome, Recovery::Finished, "it saw the create's data");
    assert_eq!(find(&db.pool, "slow").await.unwrap().id, catalog.id);
    assert_eq!(document_count(&db.pool, catalog.id).await, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_that_waited_behind_recovery_writes_nothing() {
    let db = test_db().await;
    let (catalog, pending) = half_made(&db.pool, "late").await;
    let store = CatalogDb::main_of(&db.pool);
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));

    // Recovery has the lock and has deleted the row, and stalls before commit.
    let recovery_task = {
        let (store, pending) = (store.clone(), pending.clone());
        let (entered, release) = (entered.clone(), release.clone());
        tokio::spawn(async move {
            recover_holding(&store, &pending, now(), async move {
                entered.notify_one();
                release.notified().await;
            })
            .await
        })
    };
    entered.notified().await;
    // The stalled create wakes up and runs its store step.
    let create_task = {
        let (store, catalog) = (store.clone(), catalog.clone());
        tokio::spawn(async move {
            provision_store_side(&store, &catalog, "cli", now(), std::future::ready(())).await
        })
    };
    let waited = wait_for_waiter_or_end(&db.pool, &create_task).await;
    release.notify_one();

    assert_eq!(recovery_task.await.unwrap().unwrap(), Recovery::Removed);
    let aborted = create_task.await.unwrap();
    assert!(waited, "the create should wait for recovery's lock");
    assert!(matches!(aborted, Err(OkfError::Busy { .. })), "{aborted:?}");
    assert!(!row_exists(&db.pool, catalog.id).await);
    assert_eq!(document_count(&db.pool, catalog.id).await, 0);
}

/// The pool rule: a create and a recovery each need one connection at a
/// time, so a pool of one is enough (`database_max_connections = 1`). The
/// timeout turns a regression into a failure instead of a hang.
#[tokio::test(flavor = "multi_thread")]
async fn create_and_recovery_work_on_a_one_connection_pool() {
    let db = test_db().await;
    let one = crate::db::connect(&db.url, 1)
        .await
        .expect("a one-connection pool");
    let limit = std::time::Duration::from_secs(20);

    let made = tokio::time::timeout(
        limit,
        create(&one, &new_catalog("solo"), &AuditActor::cli(), now()),
    )
    .await
    .expect("create finished")
    .unwrap();
    assert_eq!(document_count(&one, made.id).await, 2);

    let (_, lost) = half_made(&one, "solo-lost").await;
    let (_, stopped_catalog) = {
        let (catalog, pending) = half_made(&one, "solo-stopped").await;
        (catalog, pending)
    };
    let store = CatalogDb::main_of(&one);
    let removed = tokio::time::timeout(limit, recover(&store, &lost, now()))
        .await
        .expect("recovery finished")
        .unwrap();
    assert_eq!(removed, Recovery::Removed);
    let finished = tokio::time::timeout(limit, async {
        let catalog = find_half(&one, stopped_catalog.id).await;
        provision_store_side(&store, &catalog, "cli", now(), std::future::ready(())).await?;
        recover(&store, &stopped_catalog, now()).await
    })
    .await
    .expect("recovery finished")
    .unwrap();
    assert_eq!(finished, Recovery::Finished);
}

/// A half-made row as the `Catalog` step 2 takes, read without the
/// provisioned filter.
async fn find_half(pool: &PgPool, id: i64) -> Catalog {
    sqlx::query_as::<_, Catalog>(
        "SELECT id, name, display_name, description, generation, created_at, \
                NULL::text AS owner_team FROM catalogs WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

//! Tests for the store cleanup outbox (plan decision P13, issue #972).

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use sqlx::PgPool;

use super::*;
use crate::db::{test_db, TEST_TEAM};
use crate::store::changesets::{self, EntryRow};
use crate::store::users::tests::{key_for, seed_catalog_owned_by, seed_user};
use crate::store::users::{self, DeleteOptions};
use crate::stores::PoolSettings;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 23, 12, 0, 0).unwrap()
}

/// Limits for the tests: the database gives up on a wait after a second,
/// well inside the backstop, which a failing test would only reach after 60 s.
fn quick() -> Limits {
    Limits {
        attempt: Duration::from_secs(30),
        wait: Duration::from_secs(1),
    }
}

fn settings() -> PoolSettings {
    PoolSettings {
        max_connections: 4,
        idle_timeout: Duration::from_secs(60),
        connect_timeout: Duration::from_secs(2),
    }
}

/// The password in a `postgres://user:password@host:port` URL.
fn password_of(url: &str) -> String {
    let after_scheme = url.split("://").nth(1).expect("a URL");
    let credentials = after_scheme.split('@').next().expect("credentials");
    credentials
        .split(':')
        .nth(1)
        .unwrap_or_default()
        .to_string()
}

/// Registers `name` as a `host` store on the database `pool` is connected
/// to, with an `env` credential called `STORE_PASSWORD`.
async fn add_store_on(accounts: &PgPool, name: &str, pool: &PgPool) {
    let options = pool.connect_options();
    sqlx::query(
        "INSERT INTO stores (name, location_kind, host, port, database, credential_kind, \
         credential_ref, created_at) VALUES ($1, 'host', $2, $3, $4, 'env', 'STORE_PASSWORD', now())",
    )
    .bind(name)
    .bind(options.get_host())
    .bind(i32::from(options.get_port()))
    .bind(options.get_database().expect("a database"))
    .execute(accounts)
    .await
    .unwrap();
}

/// A registry whose `STORE_PASSWORD` is `password`.
fn registry(accounts: &PgPool, password: String) -> Stores {
    Stores::with_environment(
        accounts.clone(),
        settings(),
        Arc::new(move |name| (name == "STORE_PASSWORD").then(|| password.clone())),
    )
}

async fn count(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

/// Opens a changeset of `key` in catalog `catalog_id` on `store_pool`.
async fn open_on(store_pool: &PgPool, catalog_id: i64, key: &str) -> i64 {
    let mut tx = CatalogDb::main_of(store_pool).begin().await.unwrap();
    let opened = changesets::open(&mut tx, catalog_id, key, "writer", now())
        .await
        .unwrap();
    let entry = EntryRow {
        path: "a.md".to_string(),
        base_revision: None,
        content: Some(b"x\n".to_vec()),
        executable: false,
        replay: None,
    };
    changesets::upsert_entry(&mut tx, opened.id, &entry, now())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    opened.id
}

async fn state_of(pool: &PgPool, changeset: i64) -> String {
    sqlx::query_scalar("SELECT state FROM catalog.changesets WHERE id = $1")
        .bind(changeset)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn delete_alice(pool: &PgPool) {
    users::delete(
        pool,
        "alice",
        DeleteOptions::default(),
        &AuditActor::cli(),
        now(),
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_key_discards_its_changesets_on_every_store() {
    // Arrange: alice's key has an open changeset on `main` and another on a
    // second store, a database of its own that registers the catalog too.
    let accounts = test_db().await;
    let second = test_db().await;
    seed_user(&accounts.pool, "alice", None).await;
    seed_catalog_owned_by(&accounts.pool, TEST_TEAM).await;
    let key = key_for(&accounts.pool, "alice", "alice-laptop", false).await;
    let on_main = open_on(&accounts.pool, shared_id(&accounts.pool).await, &key).await;
    add_store_on(&accounts.pool, "second", &second.pool).await;
    let remote_id: i64 = sqlx::query_scalar(
        "INSERT INTO catalogs (name, created_at, store, provisioned_at) \
         VALUES ('remote', now(), 'second', now()) RETURNING id",
    )
    .fetch_one(&accounts.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO catalog.catalog_state (id) VALUES ($1)")
        .bind(remote_id)
        .execute(&second.pool)
        .await
        .unwrap();
    let on_second = open_on(&second.pool, remote_id, &key).await;

    // Act
    delete_alice(&accounts.pool).await;
    let owed = count(&accounts.pool, "SELECT count(*) FROM store_cleanups").await;
    let stores = registry(&accounts.pool, password_of(&second.url));
    let report = run(&stores, now()).await.unwrap();
    stores.forget_all().await;

    // Assert: one row per store, both settled, both changesets discarded.
    assert_eq!(owed, 2);
    assert_eq!(report, Report { done: 2, failed: 0 });
    assert_eq!(state_of(&accounts.pool, on_main).await, "discarded");
    assert_eq!(state_of(&second.pool, on_second).await, "discarded");
    assert_eq!(
        count(&accounts.pool, "SELECT count(*) FROM store_cleanups").await,
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_retries_an_unreachable_store_without_blocking_others() {
    // Arrange: a deleted key owes `main` and `down`; `down` has no
    // credential in the environment, so it cannot be opened.
    let db = test_db().await;
    seed_user(&db.pool, "alice", None).await;
    seed_catalog_owned_by(&db.pool, TEST_TEAM).await;
    let key = key_for(&db.pool, "alice", "alice-laptop", false).await;
    let changeset = open_on(&db.pool, shared_id(&db.pool).await, &key).await;
    sqlx::query(
        "INSERT INTO stores (name, location_kind, host, port, database, credential_kind, \
         credential_ref, created_at) \
         VALUES ('down', 'host', 'db.example.internal', 5432, 'okf', 'env', 'UNSET_PASSWORD', now())",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    delete_alice(&db.pool).await;
    let stores = Stores::with_environment(db.pool.clone(), settings(), Arc::new(|_| None));

    // Act
    let first = run(&stores, now()).await.unwrap();
    let second = run(&stores, now()).await.unwrap();
    stores.forget_all().await;

    // Assert: main was cleaned in the first run; `down` stays, one more
    // failed try per run, and nothing else was held up.
    assert_eq!(first, Report { done: 1, failed: 1 });
    assert_eq!(second, Report { done: 0, failed: 1 });
    assert_eq!(state_of(&db.pool, changeset).await, "discarded");
    let left: Vec<(String, i32)> = sqlx::query_as("SELECT store, attempts FROM store_cleanups")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(left, [("down".to_string(), 2)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn rows_that_keep_failing_do_not_starve_the_rest_of_their_store() {
    // Arrange: 100 deleted catalogs whose cleanup always fails, and a 101st
    // that would succeed, one more than a run takes.
    let db = test_db().await;
    sqlx::query(
        "INSERT INTO catalog.catalog_state (id) SELECT g FROM generate_series(1000, 1099) g \
         UNION ALL SELECT 2000",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO store_cleanups (store, kind, subject, created_at) \
         SELECT 'main', 'catalog', g::text, now() FROM generate_series(1000, 1099) g \
         UNION ALL SELECT 'main', 'catalog', '2000', now()",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    crate::http::api::testing::fail_on(
        &db.pool,
        "BEFORE DELETE ON catalog.catalog_state FOR EACH ROW WHEN (OLD.id < 2000)",
    )
    .await;

    // Act
    let first = settle_for_tests(&db.pool).await;
    let second = settle_for_tests(&db.pool).await;

    // Assert: the first run spends itself on the failing rows; the second
    // takes the row that has not failed first.
    assert_eq!(
        first,
        Report {
            done: 0,
            failed: 100
        }
    );
    assert_eq!(second.done, 1);
    assert_eq!(
        count(
            &db.pool,
            "SELECT count(*) FROM catalog.catalog_state WHERE id = 2000"
        )
        .await,
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_deletion_crashing_after_commit_still_cleans_up_its_changesets() {
    // Arrange: the deletion commits, and the process dies before the job:
    // its pool is closed, and only the database's rows remain.
    let db = test_db().await;
    seed_user(&db.pool, "alice", None).await;
    seed_catalog_owned_by(&db.pool, TEST_TEAM).await;
    let key = key_for(&db.pool, "alice", "alice-laptop", false).await;
    let changeset = open_on(&db.pool, shared_id(&db.pool).await, &key).await;
    let doomed = PgPool::connect_with(db.pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    delete_alice(&doomed).await;
    doomed.close().await;
    assert_eq!(state_of(&db.pool, changeset).await, "open");

    // Act: a new process's first run.
    let report = settle_for_tests(&db.pool).await;

    // Assert
    assert_eq!(report, Report { done: 1, failed: 0 });
    assert_eq!(state_of(&db.pool, changeset).await, "discarded");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_cleanup_keeps_the_stores_history_and_clears_the_key_from_it() {
    // Arrange: the deleted key's published changeset and its maintenance
    // run both name it, and both rows are there before the cleanup.
    let db = test_db().await;
    seed_user(&db.pool, "alice", None).await;
    seed_catalog_owned_by(&db.pool, TEST_TEAM).await;
    let key = key_for(&db.pool, "alice", "alice-laptop", false).await;
    let catalog_id = shared_id(&db.pool).await;
    let published = open_on(&db.pool, catalog_id, &key).await;
    sqlx::query("UPDATE catalog.changesets SET state = 'published' WHERE id = $1")
        .bind(published)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO catalog.maintenance_runs (id, catalog_id, key_id, credential_id, actor, \
         state, deadline, created_at, primary_count) \
         VALUES ('run-1', $1, $2, $2, 'a', 'running', $3, $3, 1)",
    )
    .bind(catalog_id)
    .bind(&key)
    .bind(now())
    .execute(&db.pool)
    .await
    .unwrap();
    let naming_the_key = |sql: &'static str| {
        let pool = db.pool.clone();
        let key = key.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .bind(key)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    const CHANGESETS: &str = "SELECT count(*) FROM catalog.changesets WHERE key_id = $1";
    const RUNS: &str = "SELECT count(*) FROM catalog.maintenance_runs WHERE key_id = $1";
    assert_eq!(naming_the_key(CHANGESETS).await, 1);
    assert_eq!(naming_the_key(RUNS).await, 1);
    delete_alice(&db.pool).await;

    // Act
    settle_for_tests(&db.pool).await;

    // Assert: both rows are kept, as history, and no longer name the key.
    assert_eq!(naming_the_key(CHANGESETS).await, 0);
    assert_eq!(naming_the_key(RUNS).await, 0);
    assert_eq!(state_of(&db.pool, published).await, "published");
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM catalog.maintenance_runs").await,
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_cleanup_discards_the_keys_open_changeset_and_clears_the_key() {
    // Arrange
    let db = test_db().await;
    seed_user(&db.pool, "alice", None).await;
    seed_catalog_owned_by(&db.pool, TEST_TEAM).await;
    let key = key_for(&db.pool, "alice", "alice-laptop", false).await;
    let open = open_on(&db.pool, shared_id(&db.pool).await, &key).await;
    delete_alice(&db.pool).await;

    // Act
    settle_for_tests(&db.pool).await;

    // Assert
    assert_eq!(state_of(&db.pool, open).await, "discarded");
    assert_eq!(
        count(
            &db.pool,
            "SELECT count(*) FROM catalog.changesets WHERE key_id IS NOT NULL"
        )
        .await,
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cleanup_attempt_stuck_on_a_lock_fails_without_holding_up_another_store() {
    // Arrange: alice's key has an open changeset on `main` and another on a
    // second store; another connection holds the `main` one under
    // `FOR UPDATE`, so its discard can only wait.
    let accounts = test_db().await;
    let second = test_db().await;
    seed_user(&accounts.pool, "alice", None).await;
    seed_catalog_owned_by(&accounts.pool, TEST_TEAM).await;
    let key = key_for(&accounts.pool, "alice", "alice-laptop", false).await;
    let on_main = open_on(&accounts.pool, shared_id(&accounts.pool).await, &key).await;
    add_store_on(&accounts.pool, "second", &second.pool).await;
    let remote_id: i64 = sqlx::query_scalar(
        "INSERT INTO catalogs (name, created_at, store, provisioned_at) \
         VALUES ('remote', now(), 'second', now()) RETURNING id",
    )
    .fetch_one(&accounts.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO catalog.catalog_state (id) VALUES ($1)")
        .bind(remote_id)
        .execute(&second.pool)
        .await
        .unwrap();
    let on_second = open_on(&second.pool, remote_id, &key).await;
    delete_alice(&accounts.pool).await;
    let mut holder = accounts.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM catalog.changesets WHERE id = $1 FOR UPDATE")
        .bind(on_main)
        .execute(&mut *holder)
        .await
        .unwrap();
    let stores = registry(&accounts.pool, password_of(&second.url));

    // Act: `main` sorts first, so it is attempted (and abandoned) before
    // `second`. The outer timeout only fails the test if the run hangs.
    let report = tokio::time::timeout(
        Duration::from_secs(60),
        run_bounded(&stores, now(), quick()),
    )
    .await
    .expect("the run is bounded by the attempt timeout")
    .unwrap();

    // Assert: the stuck row counts a failed try and stays; the other
    // store's row finished.
    assert_eq!(report, Report { done: 1, failed: 1 });
    assert_eq!(state_of(&second.pool, on_second).await, "discarded");
    let left: Vec<(String, i32)> = sqlx::query_as("SELECT store, attempts FROM store_cleanups")
        .fetch_all(&accounts.pool)
        .await
        .unwrap();
    assert_eq!(left, [("main".to_string(), 1)]);

    // Once the lock is released the next run settles it.
    holder.rollback().await.unwrap();
    let again = tokio::time::timeout(Duration::from_secs(60), run(&stores, now()))
        .await
        .expect("the run is bounded")
        .unwrap();
    stores.forget_all().await;
    assert_eq!(again, Report { done: 1, failed: 0 });
    assert_eq!(state_of(&accounts.pool, on_main).await, "discarded");
}

/// Two cleanup rows, `main`'s for catalog 1000 and `second`'s for catalog
/// 2000, each with a `catalog_state` row on its own store.
async fn two_catalog_cleanups(accounts: &PgPool, second: &PgPool) {
    add_store_on(accounts, "second", second).await;
    for pool in [accounts, second] {
        sqlx::query("INSERT INTO catalog.catalog_state (id) VALUES (1000), (2000)")
            .execute(pool)
            .await
            .unwrap();
    }
    sqlx::query(
        "INSERT INTO store_cleanups (store, kind, subject, created_at) \
         VALUES ('main', 'catalog', '1000', now()), ('second', 'catalog', '2000', now())",
    )
    .execute(accounts)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_store_row_lock_fails_that_cleanup_at_the_database_and_another_store_settles() {
    // Arrange: another connection holds `main`'s `catalog_state` row for
    // catalog 1000, so deleting it can only wait.
    let accounts = test_db().await;
    let second = test_db().await;
    two_catalog_cleanups(&accounts.pool, &second.pool).await;
    let mut holder = accounts.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM catalog.catalog_state WHERE id = 1000 FOR UPDATE")
        .execute(&mut *holder)
        .await
        .unwrap();
    let stores = registry(&accounts.pool, password_of(&second.url));

    // Act
    let report = tokio::time::timeout(
        Duration::from_secs(60),
        run_bounded(&stores, now(), quick()),
    )
    .await
    .expect("the run is bounded")
    .unwrap();
    stores.forget_all().await;

    // Assert: `main`'s row failed (a try counted, row kept, its data still
    // there) while `second`'s settled.
    assert_eq!(report, Report { done: 1, failed: 1 });
    let left: Vec<(String, i32)> = sqlx::query_as("SELECT store, attempts FROM store_cleanups")
        .fetch_all(&accounts.pool)
        .await
        .unwrap();
    assert_eq!(left, [("main".to_string(), 1)]);
    assert_eq!(
        count(
            &second.pool,
            "SELECT count(*) FROM catalog.catalog_state WHERE id = 2000"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &accounts.pool,
            "SELECT count(*) FROM catalog.catalog_state WHERE id = 1000"
        )
        .await,
        1
    );
    holder.rollback().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_default_key_lock_fails_a_catalog_cleanup_and_leaves_a_one_connection_pool_usable() {
    // Arrange: the accounts pool has one connection. A key whose default
    // catalog is 1000 is held `FOR UPDATE` by another connection, which the
    // catalog cleanup's account half needs.
    let accounts = test_db().await;
    let second = test_db().await;
    two_catalog_cleanups(&accounts.pool, &second.pool).await;
    seed_user(&accounts.pool, "alice", None).await;
    seed_catalog_owned_by(&accounts.pool, TEST_TEAM).await;
    let key = key_for(&accounts.pool, "alice", "alice-laptop", false).await;
    sqlx::query("UPDATE api_keys SET default_catalog_id = (SELECT id FROM catalogs WHERE name = 'shared') WHERE id = $1")
        .bind(&key)
        .execute(&accounts.pool)
        .await
        .unwrap();
    let shared = shared_id(&accounts.pool).await;
    sqlx::query("UPDATE store_cleanups SET subject = $1 WHERE store = 'main'")
        .bind(shared.to_string())
        .execute(&accounts.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO catalog.catalog_state (id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(shared)
        .execute(&accounts.pool)
        .await
        .unwrap();
    let mut holder = accounts.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM api_keys WHERE id = $1 FOR UPDATE")
        .bind(&key)
        .execute(&mut *holder)
        .await
        .unwrap();
    let one_connection = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(accounts.pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let stores = registry(&one_connection, password_of(&second.url));

    // Act
    let report = tokio::time::timeout(
        Duration::from_secs(60),
        run_bounded(&stores, now(), quick()),
    )
    .await
    .expect("the run is bounded")
    .unwrap();

    // Assert: the attempt failed and was counted (so the one connection was
    // free again to count it), `second` settled, and the pool still works
    // while the holder keeps its lock.
    assert_eq!(report, Report { done: 1, failed: 1 });
    let attempts: i32 =
        sqlx::query_scalar("SELECT attempts FROM store_cleanups WHERE store = 'main'")
            .fetch_one(&accounts.pool)
            .await
            .unwrap();
    assert_eq!(attempts, 1);
    let usable = tokio::time::timeout(
        Duration::from_secs(10),
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&one_connection),
    )
    .await
    .expect("the one connection is free")
    .unwrap();
    assert_eq!(usable, 1);
    holder.rollback().await.unwrap();
    stores.forget_all().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn owing_a_cleanup_twice_writes_one_row() {
    // Arrange
    let db = test_db().await;
    let keys = ["k1".to_string(), "k2".to_string()];
    let mut conn = db.pool.acquire().await.unwrap();

    // Act
    owe_keys(&mut conn, &keys, now()).await.unwrap();
    owe_keys(&mut conn, &keys, now()).await.unwrap();
    owe_keys(&mut conn, &[], now()).await.unwrap();

    // Assert: one row per key per store (just `main`).
    drop(conn);
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM store_cleanups").await,
        2
    );
}

/// Every filter on `provisioned_at IS NOT NULL` in the sources also checks
/// `deleted_at IS NULL`: a deleted catalog stays in the registry until its
/// cleanup, and no reader may see it. A select-list use (`..., provisioned_at
/// IS NOT NULL FROM`) is not a filter.
#[test]
fn every_registry_filter_on_provisioned_also_filters_deleted() {
    const NEEDLE: &str = "provisioned_at IS NOT NULL";
    let mut missing = Vec::new();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (at, _) in text.match_indices(NEEDLE) {
                let before = &text[..at];
                let after = text[at + NEEDLE.len()..]
                    .trim_start_matches(|c: char| c.is_whitespace() || c == '\\');
                let select_list = before.ends_with(", ") || before.ends_with("SELECT ");
                let filtered = after.starts_with("AND ")
                    && after[..after.len().min(40)].contains("deleted_at IS NULL");
                if !select_list && !filtered {
                    let line = before.matches('\n').count() + 1;
                    missing.push(format!("{}:{line}", path.display()));
                }
            }
        }
    }
    assert!(missing.is_empty(), "no deleted_at filter at {missing:?}");
}

/// The shared test catalog's id.
async fn shared_id(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT id FROM catalogs WHERE name = 'shared'")
        .fetch_one(pool)
        .await
        .unwrap()
}

//! Tests for deleting a catalog in two steps (plan decisions P12 and P13,
//! issue #972): the delete marks the catalog and owes the cleanup, and
//! `CleanupStores` removes the data and then the registry row.

use chrono::{DateTime, TimeZone, Utc};
use sqlx::PgPool;

use super::tests::AWAITED_LOCKS;
use super::*;
use crate::db::{test_db, TEST_TEAM};
use crate::labels::DisplayName;
use crate::store::audit::{Action as AuditAction, AuditActor, CatalogEntry};
use crate::store::changesets::{self, EntryRow, CATALOG_GONE};
use crate::store::cleanups;

/// Waits (polling `pg_locks`, no fixed sleep before a check) until a lock
/// request is blocked in this test's database. The wait is generous: opening
/// the store's pool can be slow on a busy machine.
async fn until_a_lock_is_awaited(pool: &PgPool) {
    let waited = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(AWAITED_LOCKS)
                .fetch_one(pool)
                .await
                .unwrap();
            if waiting >= 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "no lock request was ever blocked");
}

/// Awaits a spawned task for at most a minute, so a lock cycle fails the test
/// where it would otherwise hang it.
async fn finish<T>(task: tokio::task::JoinHandle<T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(60), task)
        .await
        .expect("the task finished: no lock cycle")
        .expect("the task did not panic")
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 23, 12, 0, 0).unwrap()
}

/// Creates catalog `name` owned by the fixture team.
async fn seed(pool: &PgPool, name: &str) -> Catalog {
    try_seed(pool, name).await.unwrap()
}

/// [`seed`], returning the error instead of panicking.
async fn try_seed(pool: &PgPool, name: &str) -> Result<Catalog, OkfError> {
    create(
        pool,
        &NewCatalog {
            name,
            display_name: &DisplayName::default(),
            description: "",
            owner_team: TEST_TEAM,
        },
        &AuditActor::cli(),
        now(),
    )
    .await
}

async fn count(pool: &PgPool, sql: &'static str, id: i64) -> i64 {
    // A table of schema `catalog` is on the store, a database of its own in
    // `OKF_TEST_STORE=separate` mode.
    let pool = if sql.contains(" catalog.") {
        crate::db::store_pool(pool)
    } else {
        pool.clone()
    };
    sqlx::query_scalar(sql)
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap()
}

/// Opens a changeset in catalog `id` the way a writer does (the catalog's
/// lock first) and writes one entry, leaving the transaction open.
async fn write_entry(
    store: &CatalogDb,
    id: i64,
) -> (sqlx::Transaction<'static, sqlx::Postgres>, i64) {
    let mut tx = store.begin().await.unwrap();
    changesets::lock_catalog(&mut tx, id).await.unwrap();
    let changeset = changesets::open(&mut tx, id, "key-1", "writer", now())
        .await
        .unwrap();
    let entry = EntryRow {
        path: "a.md".to_string(),
        base_revision: None,
        content: Some(b"x\n".to_vec()),
        executable: false,
        replay: None,
    };
    changesets::upsert_entry(&mut tx, changeset.id, &entry, now())
        .await
        .unwrap();
    (tx, changeset.id)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_catalog_disappears_before_its_store_data_is_gone() {
    // Arrange
    let db = test_db().await;
    let pg = seed(&db.pool, "pg").await;

    // Act
    delete(&db.pool, "pg", false, &AuditActor::cli(), now())
        .await
        .unwrap();

    // Assert: no lookup finds it; its data is still on the store, marked;
    // and the cleanup is owed.
    assert!(find(&db.pool, "pg").await.is_err());
    assert!(list(&db.pool).await.unwrap().is_empty());
    let marked: bool = sqlx::query_scalar(
        "SELECT deleted_at IS NOT NULL FROM catalog.catalog_state WHERE id = $1",
    )
    .bind(pg.id)
    .fetch_one(&crate::db::store_pool(&db.pool))
    .await
    .unwrap();
    assert!(marked);
    let documents = count(
        &db.pool,
        "SELECT count(*) FROM catalog.documents WHERE catalog_id = $1",
        pg.id,
    )
    .await;
    assert!(documents > 0, "the data waits for the cleanup");
    let owed: Vec<(String, String, String)> =
        sqlx::query_as("SELECT store, kind, subject FROM store_cleanups")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        owed,
        [("main".to_string(), "catalog".to_string(), pg.id.to_string())]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_removes_the_store_data_then_the_registry_row() {
    // Arrange
    let db = test_db().await;
    let pg = seed(&db.pool, "pg").await;
    delete(&db.pool, "pg", false, &AuditActor::cli(), now())
        .await
        .unwrap();

    // Act
    let report = cleanups::settle_for_tests(&db.pool).await;

    // Assert
    assert_eq!(report.done, 1);
    for sql in [
        "SELECT count(*) FROM catalog.catalog_state WHERE id = $1",
        "SELECT count(*) FROM catalog.documents WHERE catalog_id = $1",
        "SELECT count(*) FROM catalog.revisions WHERE catalog_id = $1",
        "SELECT count(*) FROM catalogs WHERE id = $1",
    ] {
        assert_eq!(count(&db.pool, sql, pg.id).await, 0, "{sql}");
    }
    let owed: i64 = sqlx::query_scalar("SELECT count(*) FROM store_cleanups")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(owed, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_catalogs_name_is_reserved_until_cleanup() {
    // Arrange
    let db = test_db().await;
    seed(&db.pool, "pg").await;
    delete(&db.pool, "pg", false, &AuditActor::cli(), now())
        .await
        .unwrap();
    // Act
    let before = try_seed(&db.pool, "pg").await;
    cleanups::settle_for_tests(&db.pool).await;
    let after = try_seed(&db.pool, "pg").await;

    // Assert
    assert!(
        matches!(&before, Err(OkfError::Refused { reason }) if reason.contains("already exists")),
        "{before:?}"
    );
    assert!(after.is_ok(), "{after:?}");
}

/// P5: the catalog's audit entries outlive the catalog. Two things keep them:
/// the cleanup never touches `catalog_audit_entries` (it has no foreign key to
/// `catalog_state`), and the table's append-only guard refuses a delete inside
/// the retention period. This test checks the outcome, not which of the two
/// held.
#[tokio::test(flavor = "multi_thread")]
async fn catalog_audit_survives_catalog_deletion() {
    // Arrange: one entry written on the store for the catalog (plan P5).
    let db = test_db().await;
    let pg = seed(&db.pool, "pg").await;
    let store = CatalogDb::main_of(&db.pool);
    let actor = AuditActor::cli();
    let mut account = db.pool.begin().await.unwrap();
    let resolved = audit::resolve_actor(&mut account, &actor, pg.id)
        .await
        .unwrap();
    let mut tx = store.begin().await.unwrap();
    audit::record_catalog(
        &mut tx,
        &resolved,
        &CatalogEntry {
            action: AuditAction::CatalogPublished,
            catalog_id: pg.id,
            catalog: "pg",
            details: serde_json::json!({}),
        },
        now(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    account.commit().await.unwrap();
    let entries = || {
        count(
            &db.pool,
            "SELECT count(*) FROM catalog.catalog_audit_entries WHERE catalog_id = $1",
            pg.id,
        )
    };
    assert_eq!(entries().await, 1);

    // Act
    delete(&db.pool, "pg", false, &AuditActor::cli(), now())
        .await
        .unwrap();
    cleanups::settle_for_tests(&db.pool).await;

    // Assert: the data went, the audit stayed.
    assert_eq!(
        count(
            &db.pool,
            "SELECT count(*) FROM catalog.catalog_state WHERE id = $1",
            pg.id
        )
        .await,
        0
    );
    assert_eq!(entries().await, 1);
}

/// The two orders of an edit and a delete (plan P12). The delete is paused
/// after its store locks; an edit started meanwhile queues behind the
/// `FOR UPDATE`, and fails once the delete commits. An edit that landed
/// first holds the delete off and is seen by it.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_racing_a_delete_either_lands_first_and_blocks_it_or_fails_after() {
    // Arrange: the delete first.
    let db = test_db().await;
    let pg = seed(&db.pool, "pg").await;
    let identity: String = sqlx::query_scalar(delete_pause::SERVER_IDENTITY)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let pause = delete_pause::set(&identity, pg.id);
    let pool = db.pool.clone();
    let deleting =
        tokio::spawn(async move { delete(&pool, "pg", false, &AuditActor::cli(), now()).await });
    delete_pause::Pause::meet(&pause.reached).await;
    let store = CatalogDb::main_of(&db.pool);
    let editing = tokio::spawn(async move {
        let mut tx = store.begin().await.unwrap();
        changesets::lock_catalog(&mut tx, pg.id).await
    });
    until_a_lock_is_awaited(&db.pool).await;

    // Act
    delete_pause::Pause::meet(&pause.resume).await;
    let deleted = finish(deleting).await;
    let edited = finish(editing).await;

    // Assert: the edit that queued behind the delete finds the catalog gone.
    deleted.unwrap();
    assert!(
        matches!(&edited, Err(OkfError::Refused { reason }) if reason == CATALOG_GONE),
        "{edited:?}"
    );

    // Arrange: the edit first, on another catalog.
    let kept = seed(&db.pool, "kept").await;
    let (writing, changeset) = write_entry(&CatalogDb::main_of(&db.pool), kept.id).await;
    let pool = db.pool.clone();
    let deleting =
        tokio::spawn(async move { delete(&pool, "kept", false, &AuditActor::cli(), now()).await });
    until_a_lock_is_awaited(&db.pool).await;

    // Act
    writing.commit().await.unwrap();
    let refused = finish(deleting).await;

    // Assert: the delete saw the edit and refused; nothing was lost.
    assert!(
        matches!(&refused, Err(OkfError::Refused { reason }) if reason.contains("--force")),
        "{refused:?}"
    );
    let state: String = sqlx::query_scalar("SELECT state FROM catalog.changesets WHERE id = $1")
        .bind(changeset)
        .fetch_one(&crate::db::store_pool(&db.pool))
        .await
        .unwrap();
    assert_eq!(state, "open");
    assert!(find(&db.pool, "kept").await.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delete_without_force_never_discards_work_written_during_it() {
    // Arrange: an edit in flight (its entry uncommitted) when the delete
    // starts, so the delete waits for it.
    let db = test_db().await;
    let pg = seed(&db.pool, "pg").await;
    let (writing, changeset) = write_entry(&CatalogDb::main_of(&db.pool), pg.id).await;
    let pool = db.pool.clone();
    let deleting =
        tokio::spawn(async move { delete(&pool, "pg", false, &AuditActor::cli(), now()).await });
    until_a_lock_is_awaited(&db.pool).await;

    // Act
    writing.commit().await.unwrap();
    let refused = finish(deleting).await;

    // Assert: refused, and the work is still open, with its entry, on a
    // catalog that is not marked.
    assert!(
        matches!(refused, Err(OkfError::Refused { .. })),
        "{refused:?}"
    );
    let (state, entries): (String, i64) = sqlx::query_as(
        "SELECT state, (SELECT count(*) FROM catalog.changeset_entries WHERE changeset_id = c.id) \
         FROM catalog.changesets c WHERE id = $1",
    )
    .bind(changeset)
    .fetch_one(&crate::db::store_pool(&db.pool))
    .await
    .unwrap();
    assert_eq!((state.as_str(), entries), ("open", 1));
    let marked: bool = sqlx::query_scalar(
        "SELECT deleted_at IS NOT NULL FROM catalog.catalog_state WHERE id = $1",
    )
    .bind(pg.id)
    .fetch_one(&crate::db::store_pool(&db.pool))
    .await
    .unwrap();
    assert!(!marked);
    let owed: i64 = sqlx::query_scalar("SELECT count(*) FROM store_cleanups")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(owed, 0, "a refused delete owes nothing");

    // And with force the same work is discarded in the delete's transaction.
    delete(&db.pool, "pg", true, &AuditActor::cli(), now())
        .await
        .unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM catalog.changesets WHERE id = $1")
        .bind(changeset)
        .fetch_one(&crate::db::store_pool(&db.pool))
        .await
        .unwrap();
    assert_eq!(state, "discarded");
}

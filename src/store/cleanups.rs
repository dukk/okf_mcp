//! Cleanup owed on catalog stores after an account-side deletion (plan
//! decision P13, issue #972).
//!
//! A deletion on the account database (a catalog, or a user with its keys)
//! cannot touch the catalog stores in the same transaction: a store may be
//! a separate database, and may be down. So the deleting transaction writes
//! one `store_cleanups` row per subject per store, and the housekeeping
//! task `CleanupStores` ([`run`]) drains them, one store at a time:
//!
//! * kind `key`: the open changesets of the deleted key on that store are
//!   discarded and audited, and the key's id is cleared from the store's
//!   changesets and maintenance runs (the foreign keys that did this are
//!   gone since 0041);
//! * kind `catalog`: the catalog's `catalog_state` row goes, cascading to
//!   all of its data but not to `catalog_audit_entries` (plan P5), and
//!   then its registry row, so the name can be used again (plan P6).
//!
//! A store that cannot be reached is retried on the next run (`attempts`
//! counts the tries); a row past [`GIVE_UP_ATTEMPTS`] is logged at error
//! level on every run and kept. The deletion itself never waits for this.

use chrono::{DateTime, Utc};
use sqlx::PgConnection;

use crate::error::OkfError;
use crate::store::audit::{self, Action, AuditActor, CatalogEntry};
use crate::store::changesets::{self, ChangesetState};
use crate::stores::{CatalogDb, StoreName, Stores};
use crate::telemetry::subscriber::split_event;

/// The audit label of the discards a key's deletion causes: entries show the
/// actor as `system:store-cleanup`.
pub const CLEANUP_ACTOR: &str = "store-cleanup";

/// How many failed tries a row may have before every run logs it at error
/// level (it is still kept and tried).
pub const GIVE_UP_ATTEMPTS: i32 = 20;

/// Rows one store's turn takes in one run, so a long list never keeps the
/// other stores waiting.
const BATCH_PER_STORE: i64 = 100;

/// Changesets or runs cleared of a deleted key per statement.
const KEY_CLEAR_BATCH: i64 = 1000;

/// The statement clearing a deleted key from `$table`, a batch at a time (a
/// macro so `concat!` builds it at compile time).
macro_rules! clear_key {
    ($table:literal) => {
        concat!(
            "UPDATE ",
            $table,
            " SET key_id = NULL WHERE id IN (SELECT id FROM ",
            $table,
            " WHERE key_id = $1 LIMIT $2)"
        )
    };
}

/// What a cleanup row is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A deleted key; the subject is its id.
    Key,
    /// A deleted catalog; the subject is its id as text.
    Catalog,
}

impl Kind {
    /// The column value.
    ///
    /// # Returns
    /// `key` or `catalog`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line accessor")
    )]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Catalog => "catalog",
        }
    }
}

/// One `store_cleanups` row as [`run`] reads it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct Due {
    id: i64,
    kind: String,
    subject: String,
    attempts: i32,
}

/// What one run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    /// Rows finished and removed.
    pub done: u64,
    /// Rows that failed and stay for the next run.
    pub failed: u64,
}

/// Owes every store a cleanup for each of `keys`, in the caller's account
/// transaction: one row per key per store in the `stores` table.
///
/// # Arguments
/// * `conn` - the deleting account transaction.
/// * `keys` - the ids of the keys and grants being deleted.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "cleanups.owe_keys"))]
pub async fn owe_keys(
    conn: &mut PgConnection,
    keys: &[String],
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    if keys.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO store_cleanups (store, kind, subject, created_at) \
         SELECT s.name, 'key', k.id, $2 FROM stores s CROSS JOIN unnest($1::text[]) AS k(id) \
         ON CONFLICT (store, kind, subject) DO NOTHING",
    )
    .bind(keys)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Owes `store` the removal of a deleted catalog's data, in the caller's
/// account transaction.
///
/// # Arguments
/// * `conn` - the deleting account transaction.
/// * `store` - the store holding the catalog.
/// * `catalog_id` - the catalog's id.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "cleanups.owe_catalog"))]
pub async fn owe_catalog(
    conn: &mut PgConnection,
    store: &StoreName,
    catalog_id: i64,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    sqlx::query(
        "INSERT INTO store_cleanups (store, kind, subject, created_at) VALUES ($1, 'catalog', $2, $3) \
         ON CONFLICT (store, kind, subject) DO NOTHING",
    )
    .bind(store.as_str())
    .bind(catalog_id.to_string())
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Writes the cleanup of every deleted catalog that has none pending: the
/// backstop for a row lost some other way (the deleting transaction writes
/// it itself).
///
/// # Arguments
/// * `conn` - an account connection.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "cleanups.owe_orphans"))]
async fn owe_deleted_catalogs(conn: &mut PgConnection, now: DateTime<Utc>) -> Result<(), OkfError> {
    sqlx::query(
        "INSERT INTO store_cleanups (store, kind, subject, created_at) \
         SELECT store, 'catalog', id::text, $1 FROM catalogs WHERE deleted_at IS NOT NULL \
         ON CONFLICT (store, kind, subject) DO NOTHING",
    )
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Drains `store_cleanups`: for each store with rows, opens it and settles
/// up to 100 of them (`BATCH_PER_STORE`). A store that cannot be opened, or a
/// row that fails, is logged, counted in `attempts` and left for the next
/// run; it never stops another store or row.
///
/// # Arguments
/// * `stores` - the registry of stores (the account database holds the
///   rows).
/// * `now` - the server clock.
///
/// # Returns
/// How many rows finished and how many failed.
///
/// # Errors
/// [`OkfError::Database`] reading the list of rows; a failure on one row is
/// not an error here.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn run(stores: &Stores, now: DateTime<Utc>) -> Result<Report, OkfError> {
    let accounts = stores.accounts();
    let mut conn = accounts.executor().acquire().await?;
    owe_deleted_catalogs(&mut conn, now).await?;
    drop(conn);
    let names: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT store FROM store_cleanups ORDER BY store")
            .fetch_all(accounts.executor())
            .await?;
    let mut report = Report::default();
    for name in names {
        let rows: Vec<Due> = sqlx::query_as(
            "SELECT id, kind, subject, attempts FROM store_cleanups \
             WHERE store = $1 ORDER BY attempts, id LIMIT $2",
        )
        .bind(&name)
        .bind(BATCH_PER_STORE)
        .fetch_all(accounts.executor())
        .await?;
        let db = match StoreName::parse(&name) {
            Ok(store) => stores.get(&store).await,
            Err(error) => Err(error),
        };
        match &db {
            Ok(db) => {
                for row in &rows {
                    match settle(db, row, now).await {
                        Ok(()) => report.done += 1,
                        Err(error) => {
                            report.failed += 1;
                            note_failure(stores, &name, row, &error).await?;
                        }
                    }
                }
            }
            // The store cannot be opened: every row of it waits, and the
            // next store is tried.
            Err(error) => {
                for row in &rows {
                    report.failed += 1;
                    note_failure(stores, &name, row, error).await?;
                }
            }
        }
    }
    Ok(report)
}

/// Counts a failed try, and logs it: a warning, or an error once the row is
/// past [`GIVE_UP_ATTEMPTS`].
///
/// # Errors
/// [`OkfError::Database`] counting the try.
#[tracing::instrument(level = "debug", skip_all)]
async fn note_failure(
    stores: &Stores,
    store: &str,
    row: &Due,
    error: &OkfError,
) -> Result<(), OkfError> {
    sqlx::query("UPDATE store_cleanups SET attempts = attempts + 1 WHERE id = $1")
        .bind(row.id)
        .execute(stores.accounts().executor())
        .await?;
    let attempts = row.attempts + 1;
    if attempts > GIVE_UP_ATTEMPTS {
        split_event!(
            error,
            { kind = error.kind(), store, cleanup = row.kind.as_str(), attempts, },
            { %error, },
            "a store cleanup keeps failing; it is kept and tried again"
        );
    } else {
        tracing::warn!(
            kind = error.kind(),
            store,
            cleanup = row.kind.as_str(),
            attempts,
            "a store cleanup failed; it will be tried again"
        );
    }
    Ok(())
}

/// Does one cleanup on its store and removes its row.
///
/// # Errors
/// [`OkfError::Database`], or [`OkfError::StoreUnavailable`] when the store
/// fails.
#[tracing::instrument(level = "debug", skip_all, fields(okf.cleanup.kind = row.kind.as_str()))]
async fn settle(db: &CatalogDb, row: &Due, now: DateTime<Utc>) -> Result<(), OkfError> {
    match row.kind.as_str() {
        "key" => clean_key(db, &row.subject, now).await?,
        _ => {
            // A subject that is no id never names a catalog: nothing to do.
            if let Ok(id) = row.subject.parse::<i64>() {
                clean_catalog(db, id).await?;
            }
        }
    }
    sqlx::query("DELETE FROM store_cleanups WHERE id = $1")
        .bind(row.id)
        .execute(db.accounts().executor())
        .await?;
    Ok(())
}

/// Removes a deleted catalog: its data on the store, then its registry row
/// (which frees the name). Each half is safe to repeat, so a run that
/// stops between them is finished by the next.
///
/// The store's delete holds no account row and the account's holds no
/// store connection, so the acquisition order of `CatalogDb::begin` is
/// not at stake.
///
/// # Errors
/// [`OkfError::StoreUnavailable`] or [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "cleanups.clean_catalog"))]
async fn clean_catalog(db: &CatalogDb, catalog_id: i64) -> Result<(), OkfError> {
    // The cascade carries the delete through every table of the catalog.
    // `catalog_audit_entries` has no foreign key to `catalog_state` and
    // stays (plan P5).
    sqlx::query("DELETE FROM catalog_state WHERE id = $1")
        .bind(catalog_id)
        .execute(db.executor())
        .await
        .map_err(|error| db.classify(error))?;
    let mut tx = db.accounts().begin().await?;
    // The keys whose default this catalog is, which the delete clears
    // (`ON DELETE SET NULL`), in id order, as `catalogs::delete_on` did.
    sqlx::query("SELECT id FROM api_keys WHERE default_catalog_id = $1 ORDER BY id FOR UPDATE")
        .bind(catalog_id)
        .execute(&mut *tx)
        .await?;
    // `deleted_at IS NOT NULL`: never a catalog that is not deleted.
    sqlx::query("DELETE FROM catalogs WHERE id = $1 AND deleted_at IS NOT NULL")
        .bind(catalog_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Settles a deleted key on one store: discards its open changesets (each
/// audited as `catalog.changeset_discarded`, reason `key_deleted`), then
/// clears its id from the store's changesets and maintenance runs, as the
/// foreign keys' `ON DELETE SET NULL` did.
///
/// # Errors
/// [`OkfError::StoreUnavailable`] or [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "cleanups.clean_key"))]
async fn clean_key(db: &CatalogDb, key_id: &str, now: DateTime<Utc>) -> Result<(), OkfError> {
    let actor = AuditActor::system(CLEANUP_ACTOR);
    loop {
        // Each row: id, handle, catalog id. Read without a lock; each
        // discard rechecks its row under the lock.
        let open: Vec<(i64, String, i64)> = sqlx::query_as(
            "SELECT id, handle, catalog_id FROM changesets \
             WHERE key_id = $1 AND state = 'open' ORDER BY id LIMIT $2",
        )
        .bind(key_id)
        .bind(KEY_CLEAR_BATCH)
        .fetch_all(db.executor())
        .await
        .map_err(|error| db.classify(error))?;
        if open.is_empty() {
            break;
        }
        for (id, handle, catalog_id) in &open {
            discard_one(db, &actor, key_id, (*id, handle, *catalog_id), now).await?;
        }
    }
    for sql in [clear_key!("changesets"), clear_key!("maintenance_runs")] {
        loop {
            let cleared = sqlx::query(sql)
                .bind(key_id)
                .bind(KEY_CLEAR_BATCH)
                .execute(db.executor())
                .await
                .map_err(|error| db.classify(error))?
                .rows_affected();
            if cleared == 0 {
                break;
            }
        }
    }
    Ok(())
}

/// Discards one open changeset of a deleted key and records it, in the
/// order every change takes: the account transaction (which resolves the
/// entry's account ids), then the store's, which commits first.
///
/// # Arguments
/// * `db` - the store.
/// * `actor` - the cleanup's actor.
/// * `key_id` - the deleted key.
/// * `changeset` - its id, handle and catalog id, as read.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::StoreUnavailable`] or [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "cleanups.discard_one"))]
async fn discard_one(
    db: &CatalogDb,
    actor: &AuditActor,
    key_id: &str,
    changeset: (i64, &str, i64),
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let (id, handle, catalog_id) = changeset;
    let mut account_tx = db.accounts().begin().await?;
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM catalogs WHERE id = $1")
        .bind(catalog_id)
        .fetch_optional(&mut *account_tx)
        .await?;
    let resolved = audit::resolve_actor(&mut account_tx, actor, catalog_id).await?;
    let mut tx = db.begin().await?;
    // The row again under its lock: a publish or discard may have closed
    // it meanwhile.
    let files: Option<i64> = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM changeset_entries e WHERE e.changeset_id = c.id) \
         FROM changesets c WHERE c.id = $1 AND c.key_id = $2 AND c.state = 'open' FOR UPDATE",
    )
    .bind(id)
    .bind(key_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(files) = files else {
        return Ok(());
    };
    changesets::set_state(&mut tx, id, ChangesetState::Discarded, now).await?;
    // A catalog already gone from the registry has no name for an entry;
    // its data goes with its own cleanup.
    if let Some(name) = name {
        audit::record_catalog(
            &mut tx,
            &resolved,
            &CatalogEntry {
                action: Action::CatalogChangesetDiscarded,
                catalog_id,
                catalog: &name,
                details: serde_json::json!({
                    "changeset": handle,
                    "files": files,
                    "reason": "key_deleted",
                }),
            },
            now,
        )
        .await?;
    }
    tx.commit().await.map_err(|error| db.classify(error))?;
    account_tx.commit().await?;
    Ok(())
}

/// Runs [`run`] once on the `main` store inside the account database, as the
/// housekeeping task does, for the tests that hold only the account pool.
///
/// # Arguments
/// * `pool` - the account pool.
///
/// # Returns
/// What the run did.
#[cfg(test)]
pub(crate) async fn settle_for_tests(pool: &sqlx::PgPool) -> Report {
    let stores = Stores::new(
        pool.clone(),
        crate::stores::PoolSettings::from_config(&crate::config::Config::default()),
    );
    let report = run(&stores, Utc::now())
        .await
        .expect("the cleanup run reads its rows");
    stores.forget_all().await;
    report
}

#[cfg(test)]
#[path = "cleanups_tests.rs"]
mod tests;

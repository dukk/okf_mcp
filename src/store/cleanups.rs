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

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgConnection;

use crate::error::OkfError;
use crate::store::audit::{self, Action, AuditActor, CatalogEntry};
use crate::store::changesets::{self, ChangesetState};
use crate::stores::bounded::{account_tx, backstop, store_tx};
use crate::stores::{AccountDb, CatalogDb, StoreName, Stores};
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

/// The bounds on one cleanup attempt (one row on one store), shared with
/// the other background work: see [`crate::stores::bounded`].
pub use crate::stores::bounded::{Limits, LIMITS};

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
/// * `accounts` - the account database.
/// * `now` - the server clock.
/// * `wait` - the database-side bound.
///
/// # Errors
/// [`OkfError::Database`] or `Busy`.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "cleanups.owe_orphans"))]
async fn owe_deleted_catalogs(
    accounts: &AccountDb,
    now: DateTime<Utc>,
    wait: Duration,
) -> Result<(), OkfError> {
    let mut tx = account_tx(accounts, wait).await?;
    sqlx::query(
        "INSERT INTO store_cleanups (store, kind, subject, created_at) \
         SELECT store, 'catalog', id::text, $1 FROM catalogs WHERE deleted_at IS NOT NULL \
         ON CONFLICT (store, kind, subject) DO NOTHING",
    )
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Drains `store_cleanups`: for each store with rows, opens it and settles
/// up to 100 of them (`BATCH_PER_STORE`).
///
/// The rule for errors: only an error that makes the whole run impossible
/// ends it, and that is reading which stores have rows (the account
/// database cannot be reached, or cannot answer). Every other error belongs
/// to one store or one row, is logged at error or warning level with the
/// store and the row's id (never a secret), and is contained there: the run
/// goes on to the next row and the next store. That covers a store that
/// cannot be opened, a store whose rows cannot be read, a row that fails or
/// overruns [`LIMITS`], the backstop that writes missing catalog cleanups,
/// and counting a failed try itself (a row whose try cannot be counted is
/// kept as it was, and tried again on the next run).
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
/// [`OkfError::Database`] or `Busy` reading which stores have rows; a
/// failure on one store or row is not an error here.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn run(stores: &Stores, now: DateTime<Utc>) -> Result<Report, OkfError> {
    run_bounded(stores, now, LIMITS).await
}

/// [`run`] under `limits`. Every statement it issues, on the account
/// database and on a store, is in a transaction with the database-side bound
/// applied, every step is under the `attempt` backstop, and no attempt holds
/// an account connection while it takes a store's or the reverse, so a lock
/// held by someone else fails the attempt at the database and the run goes
/// on.
///
/// # Arguments
/// * `stores` - the registry of stores.
/// * `now` - the server clock.
/// * `limits` - the bounds on one attempt.
///
/// # Errors
/// As [`run`].
#[tracing::instrument(level = "debug", skip_all)]
async fn run_bounded(
    stores: &Stores,
    now: DateTime<Utc>,
    limits: Limits,
) -> Result<Report, OkfError> {
    let accounts = stores.accounts();
    // The backstop failing only delays a lost row to a later run.
    if let Err(error) = backstop(limits, owe_deleted_catalogs(&accounts, now, limits.wait)).await {
        split_event!(
            error,
            { kind = error.kind(), },
            { %error, },
            "writing the cleanups of deleted catalogs failed; the next run tries again"
        );
    }
    // The one step whose failure ends the run.
    let names = backstop(limits, stores_with_rows(&accounts, limits.wait)).await?;
    let mut report = Report::default();
    for name in &names {
        run_store(stores, name, now, limits, &mut report).await;
    }
    Ok(report)
}

/// The names of the stores with cleanup rows, in name order.
///
/// # Errors
/// [`OkfError::Database`] or `Busy`.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "cleanups.stores_with_rows"))]
async fn stores_with_rows(accounts: &AccountDb, wait: Duration) -> Result<Vec<String>, OkfError> {
    let mut tx = account_tx(accounts, wait).await?;
    let names = sqlx::query_scalar("SELECT DISTINCT store FROM store_cleanups ORDER BY store")
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(names)
}

/// Up to `BATCH_PER_STORE` rows of `store`, the least tried first.
///
/// # Errors
/// [`OkfError::Database`] or `Busy`.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "cleanups.rows_of"))]
async fn rows_of(accounts: &AccountDb, store: &str, wait: Duration) -> Result<Vec<Due>, OkfError> {
    let mut tx = account_tx(accounts, wait).await?;
    let rows = sqlx::query_as(
        "SELECT id, kind, subject, attempts FROM store_cleanups \
         WHERE store = $1 ORDER BY attempts, id LIMIT $2",
    )
    .bind(store)
    .bind(BATCH_PER_STORE)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// One store's turn in a run: reads its rows, opens it and settles each row,
/// adding to `report`. Every error here is logged and contained (see
/// [`run`]).
///
/// # Arguments
/// * `stores` - the registry of stores.
/// * `name` - the store's name, as the rows hold it.
/// * `now` - the server clock.
/// * `limits` - the bounds on one attempt.
/// * `report` - the run's tally.
#[tracing::instrument(level = "debug", skip_all, fields(okf.store = name))]
async fn run_store(
    stores: &Stores,
    name: &str,
    now: DateTime<Utc>,
    limits: Limits,
    report: &mut Report,
) {
    let rows = match backstop(limits, rows_of(&stores.accounts(), name, limits.wait)).await {
        Ok(rows) => rows,
        Err(error) => {
            split_event!(
                error,
                { kind = error.kind(), store = name, },
                { %error, },
                "reading a store's cleanups failed; the next run tries again"
            );
            return;
        }
    };
    // Opening an uncached store reads its registry row (bounded at the
    // database by `store_connect_timeout`) and connects; the backstop covers
    // the whole open, so it never holds up the next store (issue #1006).
    let db = match StoreName::parse(name) {
        Ok(store) => backstop(limits, stores.get(&store)).await,
        Err(error) => Err(error),
    };
    let db = match db {
        Ok(db) => db,
        // The store cannot be opened: every row of it waits.
        Err(error) => {
            for row in &rows {
                report.failed += 1;
                note_failure(stores, name, row, &error, limits).await;
            }
            return;
        }
    };
    let attempt = Attempt {
        db: &db,
        now,
        wait: limits.wait,
    };
    for row in &rows {
        // Past the backstop the store stopped answering.
        let outcome = tokio::time::timeout(limits.attempt, attempt.settle(row))
            .await
            .unwrap_or_else(|_| {
                Err(OkfError::StoreUnavailable {
                    store: name.to_string(),
                })
            });
        match outcome {
            Ok(()) => report.done += 1,
            Err(error) => {
                report.failed += 1;
                note_failure(stores, name, row, &error, limits).await;
            }
        }
    }
}

/// Counts a failed try of `row` and logs it: a warning, or an error once
/// the row is past [`GIVE_UP_ATTEMPTS`]. Counting can fail too (the row is
/// locked, the account database stops answering): that is logged at error
/// level and the row is kept as it was, to be tried again on the next run;
/// it never ends the run (see [`run`]).
///
/// # Arguments
/// * `stores` - the registry of stores.
/// * `store` - the row's store.
/// * `row` - the row that failed.
/// * `error` - why it failed.
/// * `limits` - the bounds on counting it.
#[tracing::instrument(level = "debug", skip_all)]
async fn note_failure(stores: &Stores, store: &str, row: &Due, error: &OkfError, limits: Limits) {
    let counted = backstop(
        limits,
        count_failure(&stores.accounts(), row.id, limits.wait),
    )
    .await;
    let attempts = row.attempts + 1;
    match counted {
        Err(count_error) => split_event!(
            error,
            {
                kind = error.kind(),
                count_kind = count_error.kind(),
                store,
                cleanup = row.kind.as_str(),
                cleanup_id = row.id,
                attempts = row.attempts,
            },
            { %error, %count_error, },
            "a store cleanup failed and its try could not be counted; it is kept and tried again"
        ),
        Ok(()) if attempts > GIVE_UP_ATTEMPTS => split_event!(
            error,
            { kind = error.kind(), store, cleanup = row.kind.as_str(), cleanup_id = row.id, attempts, },
            { %error, },
            "a store cleanup keeps failing; it is kept and tried again"
        ),
        Ok(()) => tracing::warn!(
            kind = error.kind(),
            store,
            cleanup = row.kind.as_str(),
            cleanup_id = row.id,
            attempts,
            "a store cleanup failed; it will be tried again"
        ),
    }
}

/// Adds one to the `attempts` of cleanup row `id`.
///
/// # Errors
/// [`OkfError::Database`] or `Busy`.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "cleanups.count_failure"))]
async fn count_failure(accounts: &AccountDb, id: i64, wait: Duration) -> Result<(), OkfError> {
    let mut tx = account_tx(accounts, wait).await?;
    sqlx::query("UPDATE store_cleanups SET attempts = attempts + 1 WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// What one cleanup attempt works with: the store, the server clock and the
/// database-side bound its transactions carry.
#[derive(Debug, Clone, Copy)]
struct Attempt<'a> {
    db: &'a CatalogDb,
    now: DateTime<Utc>,
    wait: Duration,
}

impl Attempt<'_> {
    /// Does one cleanup on its store and removes its row.
    ///
    /// # Errors
    /// [`OkfError::Database`], or [`OkfError::StoreUnavailable`] when the
    /// store fails.
    #[tracing::instrument(level = "debug", skip_all, fields(okf.cleanup.kind = row.kind.as_str()))]
    async fn settle(&self, row: &Due) -> Result<(), OkfError> {
        match row.kind.as_str() {
            "key" => self.clean_key(&row.subject).await?,
            _ => {
                // A subject that is no id never names a catalog: nothing to do.
                if let Ok(id) = row.subject.parse::<i64>() {
                    self.clean_catalog(id).await?;
                }
            }
        }
        let mut tx = account_tx(self.db.accounts(), self.wait).await?;
        sqlx::query("DELETE FROM store_cleanups WHERE id = $1")
            .bind(row.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Removes a deleted catalog: its data on the store, then its registry
    /// row (which frees the name). Each half is its own bounded transaction
    /// and safe to repeat, so a run that stops between them is finished by
    /// the next. The store half commits before the account half begins:
    /// neither is held while the other is taken.
    ///
    /// # Errors
    /// [`OkfError::StoreUnavailable`] or [`OkfError::Database`].
    #[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "cleanups.clean_catalog"))]
    async fn clean_catalog(&self, catalog_id: i64) -> Result<(), OkfError> {
        // The cascade carries the delete through every table of the catalog.
        // `catalog_audit_entries` has no foreign key to `catalog_state` and
        // stays (plan P5).
        let mut store = store_tx(self.db, self.wait).await?;
        sqlx::query("DELETE FROM catalog_state WHERE id = $1")
            .bind(catalog_id)
            .execute(&mut *store)
            .await
            .map_err(|error| self.db.classify(error))?;
        store
            .commit()
            .await
            .map_err(|error| self.db.classify(error))?;
        let mut tx = account_tx(self.db.accounts(), self.wait).await?;
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

    /// Settles a deleted key on one store: discards its open changesets
    /// (each audited as `catalog.changeset_discarded`, reason
    /// `key_deleted`), then clears its id from the store's changesets and
    /// maintenance runs, as the foreign keys' `ON DELETE SET NULL` did.
    ///
    /// # Errors
    /// [`OkfError::StoreUnavailable`] or [`OkfError::Database`].
    #[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "cleanups.clean_key"))]
    async fn clean_key(&self, key_id: &str) -> Result<(), OkfError> {
        let actor = AuditActor::system(CLEANUP_ACTOR);
        loop {
            // Each row: id, handle, catalog id. Read without a lock; each
            // discard rechecks its row under the lock.
            let mut tx = store_tx(self.db, self.wait).await?;
            let open: Vec<(i64, String, i64)> = sqlx::query_as(
                "SELECT id, handle, catalog_id FROM changesets \
                 WHERE key_id = $1 AND state = 'open' ORDER BY id LIMIT $2",
            )
            .bind(key_id)
            .bind(KEY_CLEAR_BATCH)
            .fetch_all(&mut *tx)
            .await
            .map_err(|error| self.db.classify(error))?;
            tx.commit().await.map_err(|error| self.db.classify(error))?;
            if open.is_empty() {
                break;
            }
            for (id, handle, catalog_id) in &open {
                self.discard_one(&actor, key_id, (*id, handle, *catalog_id))
                    .await?;
            }
        }
        for sql in [clear_key!("changesets"), clear_key!("maintenance_runs")] {
            loop {
                let mut tx = store_tx(self.db, self.wait).await?;
                let cleared = sqlx::query(sql)
                    .bind(key_id)
                    .bind(KEY_CLEAR_BATCH)
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| self.db.classify(error))?
                    .rows_affected();
                tx.commit().await.map_err(|error| self.db.classify(error))?;
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
    /// * `actor` - the cleanup's actor.
    /// * `key_id` - the deleted key.
    /// * `changeset` - its id, handle and catalog id, as read.
    ///
    /// # Errors
    /// [`OkfError::StoreUnavailable`] or [`OkfError::Database`].
    #[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "cleanups.discard_one"))]
    async fn discard_one(
        &self,
        actor: &AuditActor,
        key_id: &str,
        changeset: (i64, &str, i64),
    ) -> Result<(), OkfError> {
        let (id, handle, catalog_id) = changeset;
        let mut account_tx = account_tx(self.db.accounts(), self.wait).await?;
        let name: Option<String> = sqlx::query_scalar("SELECT name FROM catalogs WHERE id = $1")
            .bind(catalog_id)
            .fetch_optional(&mut *account_tx)
            .await?;
        let resolved = audit::resolve_actor(&mut account_tx, actor, catalog_id).await?;
        let mut tx = store_tx(self.db, self.wait).await?;
        // The row again under its lock: a publish or discard may have
        // closed it meanwhile.
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
        changesets::set_state(&mut tx, id, ChangesetState::Discarded, self.now).await?;
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
                self.now,
            )
            .await?;
        }
        tx.commit().await.map_err(|error| self.db.classify(error))?;
        account_tx.commit().await?;
        Ok(())
    }
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

//! Bounded waits for background database work: the store cleanup worker
//! (`store::cleanups`), the per-store housekeeping jobs (`housekeeping`) and
//! the registry's row read when a store is opened.
//!
//! Background work must never wait without limit on a lock someone else
//! holds, or on a database that stops answering: a stuck step would hold a
//! connection and keep every later store (or job) waiting. So each step has
//! two bounds ([`Limits`]):
//!
//! * **at the database**: every statement runs in a transaction that sets
//!   `lock_timeout` and `statement_timeout` (`SET LOCAL`, through
//!   `set_config(.., true)`), so PostgreSQL itself gives up, rolls the
//!   transaction back and hands the connection back, whether or not the
//!   caller is still waiting ([`account_tx`], [`store_tx`], [`bound_waits`]);
//! * **in the process**: a `tokio` timeout around the whole step
//!   ([`backstop`]), for what the database cannot bound (getting a
//!   connection, a connect, a server that stops answering). It is longer
//!   than the database bound, so that one fires first: dropping a future does
//!   not cancel the statement it sent, so the backstop alone could leave a
//!   connection busy.

use std::future::Future;
use std::time::Duration;

use sqlx::{PgConnection, Postgres, Transaction};

use crate::error::OkfError;
use crate::stores::{AccountDb, CatalogDb};

/// The two limits on one bounded step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The longest one step may take (the `tokio` backstop).
    pub attempt: Duration,
    /// `lock_timeout` and `statement_timeout` inside each transaction.
    pub wait: Duration,
}

/// The limits background work uses: 20 s at the database, 30 s per step.
pub const LIMITS: Limits = Limits {
    attempt: Duration::from_secs(30),
    wait: Duration::from_secs(20),
};

/// Runs `step` under the `attempt` backstop of `limits`: past it, the step
/// is dropped and fails with `Busy`.
///
/// # Arguments
/// * `limits` - the bounds; only `attempt` is used here.
/// * `step` - the work, which bounds its own statements with `wait`.
///
/// # Returns
/// What the step returned.
///
/// # Errors
/// The step's own error, or [`OkfError::Busy`] past the backstop.
#[tracing::instrument(level = "trace", skip_all)]
pub async fn backstop<T>(
    limits: Limits,
    step: impl Future<Output = Result<T, OkfError>>,
) -> Result<T, OkfError> {
    tokio::time::timeout(limits.attempt, step)
        .await
        .unwrap_or_else(|_| {
            Err(OkfError::Busy {
                reason: "a background database step ran past its limit".to_string(),
            })
        })
}

/// Begins an account transaction whose waits are bounded by `wait`.
///
/// # Arguments
/// * `accounts` - the account database.
/// * `wait` - the database-side bound on each lock wait and statement.
///
/// # Returns
/// The transaction (it owns its connection).
///
/// # Errors
/// [`OkfError::Database`] or `Busy`, with no connection to be had.
#[tracing::instrument(level = "trace", skip_all)]
pub async fn account_tx(
    accounts: &AccountDb,
    wait: Duration,
) -> Result<Transaction<'static, Postgres>, OkfError> {
    let mut tx = accounts.begin().await?;
    bound_waits(&mut tx, wait).await?;
    Ok(tx)
}

/// Begins a store transaction whose waits are bounded by `wait`.
///
/// # Arguments
/// * `db` - the store.
/// * `wait` - the database-side bound on each lock wait and statement.
///
/// # Returns
/// The transaction (it owns its connection).
///
/// # Errors
/// [`OkfError::StoreUnavailable`] with no connection to be had;
/// [`OkfError::Database`].
#[tracing::instrument(level = "trace", skip_all)]
pub async fn store_tx(
    db: &CatalogDb,
    wait: Duration,
) -> Result<Transaction<'static, Postgres>, OkfError> {
    let mut tx = db.begin().await?;
    bound_waits(&mut tx, wait).await?;
    Ok(tx)
}

/// Bounds the waits of the transaction `tx` is on: a lock wait or a
/// statement that runs past `wait` fails (and rolls the transaction back)
/// instead of waiting on.
///
/// # Arguments
/// * `tx` - a connection inside a transaction (`SET LOCAL` ends with it).
/// * `wait` - the bound.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "trace", skip_all)]
pub async fn bound_waits(tx: &mut PgConnection, wait: Duration) -> Result<(), OkfError> {
    sqlx::query(
        "SELECT set_config('lock_timeout', $1, true), set_config('statement_timeout', $1, true)",
    )
    .bind(format!("{}ms", wait.as_millis()))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

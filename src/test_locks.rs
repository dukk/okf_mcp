//! Test helpers that decide which heavyweight lock a backend waits on.
//!
//! They read `pg_locks`, never `pg_stat_activity.query`. `pg_stat_activity`
//! takes its query text and state from a snapshot made once per transaction,
//! while `wait_event_type` and `pg_blocking_pids` are read live, so a backend
//! that was idle when the snapshot was taken and is blocked now shows the
//! text of the statement it ran before (issues #372 and #639). `pg_locks`
//! lists the lock each waiter has requested and not been granted, read when
//! the query runs.

use std::time::Duration;

use sqlx::{PgConnection, PgPool};

/// How long a helper waits for the lock wait it looks for.
const WAIT_LIMIT: Duration = Duration::from_secs(10);

/// A session queued for a heavyweight lock that another session holds, as
/// `pg_locks` shows it.
#[derive(Debug)]
pub(crate) struct Waiter {
    /// The waiting session's backend pid.
    pub(crate) pid: i32,
    /// `pg_locks.locktype` of the lock it waits for (`advisory`,
    /// `transactionid`, `tuple`, `relation`, ...).
    pub(crate) locktype: String,
    /// For an advisory lock taken with the two-key form, the first key
    /// (`classid`); `None` for other lock types.
    pub(crate) key1: Option<i64>,
    /// For an advisory lock taken with the two-key form, the second key
    /// (`objid`); `None` for other lock types.
    pub(crate) key2: Option<i64>,
}

/// Waits (up to 10 s) until some session is queued for a lock that `holder`
/// holds, and returns that session and the lock it waits for.
///
/// # Arguments
/// * `conn` - the connection to poll from.
/// * `holder` - the backend pid holding the lock.
///
/// # Returns
/// The first waiter (lowest pid) queued behind `holder`.
///
/// # Panics
/// When no session waits on `holder` within 10 s, or a query fails.
pub(crate) async fn wait_waiter(conn: &mut PgConnection, holder: i32) -> Waiter {
    tokio::time::timeout(WAIT_LIMIT, async {
        loop {
            // `classid`/`objid` are `oid`s, cast so they decode as `i64`.
            let row: Option<(i32, String, Option<i64>, Option<i64>)> = sqlx::query_as(
                "SELECT pid, locktype, classid::int8, objid::int8 FROM pg_locks \
                 WHERE NOT granted AND $1 = ANY(pg_blocking_pids(pid)) ORDER BY pid LIMIT 1",
            )
            .bind(holder)
            .fetch_optional(&mut *conn)
            .await
            .unwrap();
            if let Some((pid, locktype, key1, key2)) = row {
                return Waiter {
                    pid,
                    locktype,
                    key1,
                    key2,
                };
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("no session queued for a lock the holder holds")
}

/// Waits until some session is queued for a row lock on `table` that
/// `holder` holds, and returns that session's pid.
///
/// A session that must wait for a row lock first takes the row's `tuple`
/// lock (granted, on the table's relation), then waits for the holder's
/// `transactionid` lock (not granted). So the wait on a row of `table` is a
/// not-granted `transactionid` lock together with a granted `tuple` lock on
/// `table`, both held up by `holder`. The `tuple` lock is the only part of
/// the wait that names the table.
///
/// # Arguments
/// * `pool` - the pool to poll from; the pids and `pg_locks` are read from
///   the database the pool connects to.
/// * `holder` - the backend pid holding the row.
/// * `table` - the table whose row is locked (any name `regclass` accepts).
/// * `limit` - how long to wait.
///
/// # Returns
/// The lowest pid among the sessions waiting, or `None` when none did
/// within `limit`.
///
/// # Panics
/// When a query fails.
pub(crate) async fn wait_row_lock_waiter(
    pool: &PgPool,
    holder: i32,
    table: &str,
    limit: Duration,
) -> Option<i32> {
    // A table of schema `catalog` lives in the store's database, which is a
    // database of its own in `OKF_TEST_STORE=separate` mode.
    let pool = &if table.starts_with("catalog.") {
        crate::db::store_pool(pool)
    } else {
        pool.clone()
    };
    tokio::time::timeout(limit, async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT w.pid FROM pg_locks w \
                 JOIN pg_locks t ON t.pid = w.pid AND t.locktype = 'tuple' AND t.granted \
                  AND t.relation = $2::regclass \
                  AND t.database = (SELECT oid FROM pg_database WHERE datname = current_database()) \
                 WHERE w.locktype = 'transactionid' AND NOT w.granted \
                  AND $1 = ANY(pg_blocking_pids(w.pid)) \
                 ORDER BY w.pid LIMIT 1",
            )
            .bind(holder)
            .bind(table)
            .fetch_optional(pool)
            .await
            .unwrap()
            .flatten();
            if let Some(pid) = pid {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .ok()
}

/// [`wait_row_lock_waiter`] for a test that needs the wait: waits up to 10 s.
///
/// # Panics
/// When no session waits for a row lock on `table` held by `holder` within
/// 10 s; the message names the table and the holder.
pub(crate) async fn expect_row_lock_waiter(pool: &PgPool, holder: i32, table: &str) -> i32 {
    wait_row_lock_waiter(pool, holder, table, WAIT_LIMIT)
        .await
        .unwrap_or_else(|| {
            panic!("no session waited for a row lock on {table} held by backend {holder}")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;

    /// Starts a session that waits for a `FOR SHARE` lock on `lock_probe_a`'s
    /// row while the returned transaction holds it `FOR UPDATE`.
    async fn hold_and_wait(
        pool: &PgPool,
    ) -> (
        sqlx::Transaction<'static, sqlx::Postgres>,
        i32,
        tokio::task::JoinHandle<()>,
    ) {
        sqlx::query("CREATE TABLE lock_probe_a (id int PRIMARY KEY)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE lock_probe_b (id int PRIMARY KEY)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO lock_probe_a VALUES (1)")
            .execute(pool)
            .await
            .unwrap();
        let mut holder = pool.begin().await.unwrap();
        let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *holder)
            .await
            .unwrap();
        sqlx::query("SELECT id FROM lock_probe_a WHERE id = 1 FOR UPDATE")
            .execute(&mut *holder)
            .await
            .unwrap();
        let waiting_pool = pool.clone();
        let waiter = tokio::spawn(async move {
            sqlx::query("SELECT id FROM lock_probe_a WHERE id = 1 FOR SHARE")
                .fetch_all(&waiting_pool)
                .await
                .unwrap();
        });
        (holder, holder_pid, waiter)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_row_lock_waiter_is_found_by_its_table() {
        let db = test_db().await;
        let (holder, holder_pid, waiter) = hold_and_wait(&db.pool).await;
        let pid = expect_row_lock_waiter(&db.pool, holder_pid, "lock_probe_a").await;
        assert_ne!(pid, holder_pid);
        holder.commit().await.unwrap();
        waiter.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_row_lock_waiter_on_another_table_does_not_match() {
        let db = test_db().await;
        let (holder, holder_pid, waiter) = hold_and_wait(&db.pool).await;
        // The waiter is queued (found by its own table), yet is no waiter
        // for the other table's rows.
        expect_row_lock_waiter(&db.pool, holder_pid, "lock_probe_a").await;
        let found = wait_row_lock_waiter(
            &db.pool,
            holder_pid,
            "lock_probe_b",
            Duration::from_millis(300),
        )
        .await;
        assert_eq!(found, None);
        holder.commit().await.unwrap();
        waiter.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_row_lock_waiter_behind_another_holder_does_not_match() {
        let db = test_db().await;
        let (holder, holder_pid, waiter) = hold_and_wait(&db.pool).await;
        expect_row_lock_waiter(&db.pool, holder_pid, "lock_probe_a").await;
        let other = holder_pid + 1;
        let found =
            wait_row_lock_waiter(&db.pool, other, "lock_probe_a", Duration::from_millis(300)).await;
        assert_eq!(found, None);
        holder.commit().await.unwrap();
        waiter.await.unwrap();
    }
}

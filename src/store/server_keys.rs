//! Server-side keys (`server_keys`, migration `0028_server_keys.sql`): 32
//! random bytes per purpose, created the first time the server needs them
//! and kept in Postgres, so every replica reads the same key and it
//! survives restarts. The only purpose so far is [`AUDIT_CURSOR`], the key
//! that seals a team owner's audit cursor (issue #430).
//!
//! Rotating a key is deleting its row: the next use creates a new one.
//! Nothing caches a key in memory, so every replica moves to the new key
//! at once, and whatever the old key sealed stops opening.

use sqlx::PgPool;

use crate::error::OkfError;

/// The key that seals a team owner's audit cursor
/// (`http::api::audit::cursor`).
pub const AUDIT_CURSOR: &str = "audit_cursor";

/// A key's length in bytes.
pub const KEY_LEN: usize = 32;

/// The key named `name`, created now if it does not exist yet.
///
/// Two servers creating it at once agree: each inserts its own random key
/// with `ON CONFLICT DO NOTHING`, then both read whichever row won.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the key's purpose, such as [`AUDIT_CURSOR`].
///
/// # Returns
/// The key's bytes.
///
/// # Errors
/// [`OkfError::Io`] if the OS random source fails; [`OkfError::Database`]
/// for a database failure, or when the row is deleted again between the
/// insert and the read (`RowNotFound`), or holds a key of the wrong length.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "server_keys.get_or_create"))]
pub async fn get_or_create(pool: &PgPool, name: &str) -> Result<[u8; KEY_LEN], OkfError> {
    const READ: &str = "SELECT key FROM server_keys WHERE name = $1";
    let stored: Option<Vec<u8>> = sqlx::query_scalar(READ)
        .bind(name)
        .fetch_optional(pool)
        .await?;
    let stored = match stored {
        Some(stored) => stored,
        None => {
            let mut fresh = [0u8; KEY_LEN];
            getrandom::fill(&mut fresh).map_err(|error| OkfError::Io {
                path: "<os random source>".to_string(),
                source: std::io::Error::other(error),
            })?;
            sqlx::query(
                "INSERT INTO server_keys (name, key) VALUES ($1, $2) \
                 ON CONFLICT (name) DO NOTHING",
            )
            .bind(name)
            .bind(fresh.as_slice())
            .execute(pool)
            .await?;
            // A new statement, so a new snapshot: it sees the row another
            // server inserted first, which `DO NOTHING` kept.
            sqlx::query_scalar(READ).bind(name).fetch_one(pool).await?
        }
    };
    // `try_from` turns the `Vec` into a fixed-size array, failing on any
    // other length (the table's CHECK rules that out).
    <[u8; KEY_LEN]>::try_from(stored).map_err(|_| OkfError::Database {
        source: sqlx::Error::Decode(format!("server key {name:?} is not {KEY_LEN} bytes").into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;

    #[tokio::test(flavor = "multi_thread")]
    async fn a_key_is_created_once_and_read_back_by_every_caller() {
        let db = test_db().await;

        // Eight first callers at once, as replicas starting together. A
        // `JoinSet` runs them as concurrent tasks.
        let mut racing = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let pool = db.pool.clone();
            racing.spawn(async move { get_or_create(&pool, AUDIT_CURSOR).await });
        }
        let keys = tokio::time::timeout(std::time::Duration::from_secs(30), racing.join_all())
            .await
            .expect("the first callers finished within 30 s");
        let keys: Vec<[u8; KEY_LEN]> = keys.into_iter().map(Result::unwrap).collect();
        let again = get_or_create(&db.pool, AUDIT_CURSOR).await.unwrap();
        let other = get_or_create(&db.pool, "another").await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM server_keys")
            .fetch_one(&db.pool)
            .await
            .unwrap();

        assert!(
            keys.iter().all(|key| *key == keys[0]),
            "every caller agrees"
        );
        assert_eq!(again, keys[0], "a later read gets the same key");
        assert_ne!(other, keys[0], "each purpose has its own key");
        assert_ne!(keys[0], [0u8; KEY_LEN]);
        assert_eq!(rows, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_a_key_rotates_it() {
        let db = test_db().await;
        let old = get_or_create(&db.pool, AUDIT_CURSOR).await.unwrap();

        sqlx::query("DELETE FROM server_keys WHERE name = $1")
            .bind(AUDIT_CURSOR)
            .execute(&db.pool)
            .await
            .unwrap();
        let new = get_or_create(&db.pool, AUDIT_CURSOR).await.unwrap();

        assert_ne!(old, new);
    }
}

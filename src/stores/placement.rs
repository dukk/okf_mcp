//! Placement: which store a new catalog goes on (plan Task 10).
//!
//! A new catalog goes on its owner team's pinned store (`teams.store`), else
//! on the default store (the `default_store` setting, `main` until an admin
//! changes it). Placement is decided once, when the catalog is created, and
//! recorded on the catalog's row: changing the default or a pin later moves
//! no existing catalog.

use sqlx::PgConnection;

use super::StoreName;
use crate::error::OkfError;
use crate::store::decode_column;

/// The `server_settings` key that names the default store.
pub const DEFAULT_STORE_KEY: &str = "default_store";

/// The store a new catalog owned by `owner_team` goes on: the team's pin if
/// it has one, else the default store.
///
/// # Arguments
/// * `conn` - the account database; a creating transaction passes its own
///   connection so the choice and the insert see one snapshot of the rows.
/// * `owner_team` - the owner team's name.
///
/// # Returns
/// The store's name.
///
/// # Errors
/// [`OkfError::Database`], or a `ColumnDecode` for a stored name that is not
/// a valid store name.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "placement.choose"))]
pub async fn choose(conn: &mut PgConnection, owner_team: &str) -> Result<StoreName, OkfError> {
    let pinned: Option<String> = sqlx::query_scalar("SELECT store FROM teams WHERE name = $1")
        .bind(owner_team)
        .fetch_optional(&mut *conn)
        .await?
        .flatten();
    match pinned {
        Some(name) => decode_column(StoreName::parse(&name).ok(), "teams.store"),
        None => default_store(conn).await,
    }
}

/// The default store: the `default_store` setting, or `main` if the row is
/// missing (migration 0036 writes it, so only a hand-edited database lacks
/// it).
///
/// # Arguments
/// * `conn` - the account database.
///
/// # Returns
/// The store's name.
///
/// # Errors
/// [`OkfError::Database`], or a `ColumnDecode` for a value that is not a
/// valid store name.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "placement.default_store"))]
pub async fn default_store(conn: impl sqlx::PgExecutor<'_>) -> Result<StoreName, OkfError> {
    let value: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT value FROM server_settings WHERE key = $1")
            .bind(DEFAULT_STORE_KEY)
            .fetch_optional(conn)
            .await?;
    let Some(value) = value else {
        return Ok(StoreName::main());
    };
    decode_column(
        value.as_str().and_then(|name| StoreName::parse(name).ok()),
        "server_settings.default_store",
    )
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sqlx::PgPool;

    use super::*;
    use crate::db::{test_db, TEST_TEAM};
    use crate::labels::DisplayName;
    use crate::store::audit::AuditActor;
    use crate::store::catalogs::{self, NewCatalog};
    use crate::store::{stores, teams};
    use crate::stores::registry::PoolSettings;
    use crate::stores::{CatalogDb, Stores};

    /// Adds a `host` store row, as a later task's `add` will, so a catalog
    /// can be placed on it. The catalog's starter files still go into the
    /// account database, which is all these tests read.
    async fn add_host_row(pool: &PgPool, name: &str) -> StoreName {
        sqlx::query(
            "INSERT INTO stores (name, location_kind, host, port, database, \
             credential_kind, credential_ref, created_at) \
             VALUES ($1, 'host', 'db.example.internal', 5432, $1, 'env', 'SOME_VAR', now())",
        )
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
        StoreName::parse(name).unwrap()
    }

    /// Creates a catalog owned by [`TEST_TEAM`] and returns the store its row
    /// records.
    async fn create_on(pool: &PgPool, name: &str) -> String {
        let new = NewCatalog {
            name,
            display_name: &DisplayName::default(),
            description: "",
            owner_team: TEST_TEAM,
        };
        // Step 1 only: where the row lands is what these tests read, and a
        // host store has no data side to create until Task 20.
        catalogs::register(pool, &new, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
        sqlx::query_scalar("SELECT store FROM catalogs WHERE name = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_new_catalog_lands_on_the_default_store() {
        let db = test_db().await;

        let before = create_on(&db.pool, "first").await;
        let shared = add_host_row(&db.pool, "shared-1").await;
        stores::set_default(&db.pool, &shared, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
        let after = create_on(&db.pool, "second").await;

        assert_eq!(before, "main");
        assert_eq!(after, "shared-1");
    }

    #[tokio::test]
    async fn a_team_pin_wins_over_the_default() {
        let db = test_db().await;
        let shared = add_host_row(&db.pool, "shared-1").await;
        let other = add_host_row(&db.pool, "shared-2").await;
        stores::set_default(&db.pool, &shared, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();

        let changed = teams::set_store(
            &db.pool,
            TEST_TEAM,
            Some(&other),
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let pinned = create_on(&db.pool, "pinned").await;
        teams::set_store(&db.pool, TEST_TEAM, None, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
        let unpinned = create_on(&db.pool, "unpinned").await;

        assert!(changed);
        assert_eq!(pinned, "shared-2");
        assert_eq!(unpinned, "shared-1");
    }

    #[tokio::test]
    async fn changing_the_default_moves_new_catalogs_only() {
        let db = test_db().await;
        let shared = add_host_row(&db.pool, "shared-1").await;
        let old = create_on(&db.pool, "old").await;

        stores::set_default(&db.pool, &shared, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
        let team_pin = teams::set_store(
            &db.pool,
            TEST_TEAM,
            Some(&shared),
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let still: String = sqlx::query_scalar("SELECT store FROM catalogs WHERE name = 'old'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let new = create_on(&db.pool, "new").await;

        assert!(team_pin);
        assert_eq!(old, "main");
        assert_eq!(still, "main");
        assert_eq!(new, "shared-1");
    }

    /// How many rows each table of schema `catalog` holds, by table name,
    /// except the migration history and the identity row, which opening a
    /// store writes whatever catalogs it holds.
    async fn catalog_row_counts(db: &CatalogDb) -> Vec<(String, i64)> {
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT table_name::text FROM information_schema.tables \
             WHERE table_schema = 'catalog' AND table_type = 'BASE TABLE' \
             AND table_name NOT IN ('_sqlx_migrations', 'store_identity') ORDER BY 1",
        )
        .fetch_all(db.executor())
        .await
        .unwrap();
        let mut counts = Vec::with_capacity(tables.len());
        for table in tables {
            // The name came from the catalog, not from a caller.
            let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT count(*) FROM catalog.\"{table}\""
            )))
            .fetch_one(db.executor())
            .await
            .unwrap();
            counts.push((table, count));
        }
        counts
    }

    #[tokio::test]
    async fn isolation_a_catalog_on_store_b_writes_nothing_to_store_a() {
        let db = test_db().await;
        // Store `b` is a database of its own, reached through the registry
        // as a `host` store; store `a` is `main`, whichever database that is.
        let database = db.extra_store_database("b").await;
        let options = db.pool.connect_options();
        sqlx::query(
            "INSERT INTO stores (name, location_kind, host, port, database, \
             credential_kind, credential_ref, created_at) \
             VALUES ('store-b', 'host', $1, $2, $3, 'env', $4, now())",
        )
        .bind(options.get_host())
        .bind(i32::from(options.get_port()))
        .bind(&database)
        .bind(crate::db::pg::STORE_PASSWORD_VAR)
        .execute(&db.pool)
        .await
        .unwrap();
        let store_b = StoreName::parse("store-b").unwrap();
        teams::set_store(
            &db.pool,
            TEST_TEAM,
            Some(&store_b),
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let stores = Stores::new(
            db.pool.clone(),
            PoolSettings::from_config(&crate::config::Config::default()),
        );
        let store_a_before =
            catalog_row_counts(&stores.get(&StoreName::main()).await.unwrap()).await;

        let new = NewCatalog {
            name: "on-b",
            display_name: &DisplayName::default(),
            description: "",
            owner_team: TEST_TEAM,
        };
        catalogs::create_in(&stores, &new, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();

        let store_a_after =
            catalog_row_counts(&stores.get(&StoreName::main()).await.unwrap()).await;
        let store_b_after = catalog_row_counts(&stores.get(&store_b).await.unwrap()).await;
        assert_eq!(store_a_after, store_a_before);
        assert!(
            store_b_after.iter().any(|(_, count)| *count > 0),
            "the catalog's files and state are on store b: {store_b_after:?}"
        );
        assert!(
            store_a_before.iter().all(|(_, count)| *count == 0),
            "store a held nothing to begin with: {store_a_before:?}"
        );
        stores.forget_all().await;
    }

    #[tokio::test]
    async fn a_missing_default_setting_falls_back_to_main() {
        let db = test_db().await;
        sqlx::query("DELETE FROM server_settings WHERE key = 'default_store'")
            .execute(&db.pool)
            .await
            .unwrap();

        assert_eq!(default_store(&db.pool).await.unwrap(), StoreName::main());
    }
}

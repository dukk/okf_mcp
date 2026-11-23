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
use crate::store::stores::StoreLocation;

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

/// Refuses a store whose database is not the account database, until
/// Task 20 enables separate stores (plan decision P8): while any catalog
/// code still assumes `main`, a catalog placed elsewhere would be broken.
///
/// This is the one place that refusal lives. **Task 20 deletes this
/// function and its one call in `store::stores::add`.**
///
/// # Arguments
/// * `location` - where the store being added keeps its database.
///
/// # Errors
/// [`OkfError::Refused`] for a `host` store.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(uninstrumented_fn, reason = "a pure check of a value, no I/O")
)]
pub fn refuse_host_stores_until_task_20(location: &StoreLocation) -> Result<(), OkfError> {
    match location {
        StoreLocation::AccountDatabase => Ok(()),
        StoreLocation::Host { .. } => Err(OkfError::Refused {
            reason: "separate stores are not enabled yet".to_string(),
        }),
    }
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
        catalogs::create(pool, &new, &AuditActor::cli(), Utc::now())
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

    #[tokio::test]
    async fn a_missing_default_setting_falls_back_to_main() {
        let db = test_db().await;
        sqlx::query("DELETE FROM server_settings WHERE key = 'default_store'")
            .execute(&db.pool)
            .await
            .unwrap();

        assert_eq!(default_store(&db.pool).await.unwrap(), StoreName::main());
    }

    #[test]
    fn only_host_stores_are_refused_until_task_20() {
        let host = StoreLocation::Host {
            host: "db.example.internal".to_string(),
            port: 5432,
            database: "okf".to_string(),
            credential_kind: crate::store::stores::CredentialKind::Env,
            credential_ref: "SOME_VAR".to_string(),
        };

        assert!(refuse_host_stores_until_task_20(&host).is_err());
        assert!(refuse_host_stores_until_task_20(&StoreLocation::AccountDatabase).is_ok());
    }
}

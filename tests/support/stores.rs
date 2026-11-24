//! Store handles for the integration tests, which hold only the account
//! pool: the `main` store is the account database, or the second database
//! in `OKF_TEST_STORE=separate` mode.

use chrono::{DateTime, Utc};
use okf_mcp::config::Config;
use okf_mcp::error::OkfError;
use okf_mcp::store::audit::AuditActor;
use okf_mcp::store::catalogs::{self, Catalog, NewCatalog};
use okf_mcp::stores::{AccountDb, CatalogDb, PoolSettings, StoreName, Stores};
use sqlx::postgres::{PgPool, PgPoolOptions};

/// The `main` store as a handle on a lazy pool of its own whose connections
/// see only schema `catalog`, as the server's store pool does (the server
/// gets its handles from `Stores`).
///
/// # Arguments
/// * `accounts` - the account pool.
///
/// # Returns
/// The handle for the store `main`.
pub fn main_store(accounts: &PgPool) -> CatalogDb {
    let pool = PgPoolOptions::new()
        .max_connections(accounts.options().get_max_connections())
        .after_connect(|connection, _meta| {
            Box::pin(async move {
                sqlx::query("SET search_path = catalog")
                    .execute(connection)
                    .await
                    .map(|_| ())
            })
        })
        .connect_lazy_with(crate::support::pg::store_options(accounts));
    CatalogDb::new(StoreName::main(), pool, AccountDb::new(accounts.clone()))
}

/// Creates a catalog the way the command line does, through a registry of
/// stores over `pool`.
///
/// # Arguments
/// * `pool` - the account pool.
/// * `new` - the catalog.
/// * `actor` - who, for the audit log.
/// * `now` - the creation time.
///
/// # Returns
/// The new row.
///
/// # Errors
/// As `catalogs::create_in`.
pub async fn create_catalog(
    pool: &PgPool,
    new: &NewCatalog<'_>,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<Catalog, OkfError> {
    let stores = Stores::new(pool.clone(), PoolSettings::from_config(&Config::default()));
    let created = catalogs::create_in(&stores, new, actor, now).await;
    stores.forget_all().await;
    created
}

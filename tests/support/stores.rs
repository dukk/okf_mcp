//! Store handles for the integration tests, which hold only the account
//! pool: the `main` store is the account database, or the second database
//! in `OKF_TEST_STORE=separate` mode.

// Each crate that includes this file uses a different part of it.
#![allow(dead_code)]

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

/// A forwarding TCP proxy in front of a store's database: the way a test
/// takes "the store's database" away after the server holds open
/// connections to it. Stopping it cuts every connection through it and
/// closes its port, so a new connect is refused, as when a database host
/// goes down.
pub struct StoreProxy {
    /// The proxy's port on `127.0.0.1`.
    pub port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl StoreProxy {
    /// Starts a proxy that forwards every connection to `target`.
    ///
    /// # Arguments
    /// * `target` - `host:port` of the database being fronted.
    ///
    /// # Returns
    /// The running proxy.
    ///
    /// # Panics
    /// If no port can be bound.
    pub async fn start(target: String) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a port");
        let port = listener.local_addr().expect("its address").port();
        let task = tokio::spawn(async move {
            // Dropping the set aborts every forwarding task, which closes
            // their sockets (a Rust task is dropped, not left running).
            let mut forwards = tokio::task::JoinSet::new();
            while let Ok((mut client, _)) = listener.accept().await {
                let target = target.clone();
                forwards.spawn(async move {
                    if let Ok(mut upstream) = tokio::net::TcpStream::connect(target).await {
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                    }
                });
            }
        });
        Self { port, task }
    }

    /// Takes the database away: every open connection is cut and the port
    /// is closed.
    pub async fn stop(self) {
        self.task.abort();
        // Finished once the listener and the forwarding tasks are dropped.
        let _ = self.task.await;
    }
}

//! [`Stores`]: every catalog store's database handle, opened on first use.
//!
//! The registry is the code behind the `stores` table. A store's pool is
//! not opened at start-up: the first request that needs the store opens it,
//! from the store's row, with its own `search_path`, size and timeouts.
//! Pools are never shared between stores, and never with the account
//! database, not even for the `main` store inside the account database
//! (plan decision P1): a store pool's `search_path` makes a query sent to
//! the wrong database fail in tests.
//!
//! # Concurrency
//!
//! The pools live in a moka cache keyed by store name. moka runs one
//! initialiser per missing key however many callers ask together (single
//! flight), the others wait for its answer, and a failed initialiser caches
//! nothing, so an unreachable store is tried again by the next request
//! instead of being poisoned. Nothing is locked while a pool connects:
//! requests for stores that are already open never wait for a store that is
//! still opening. A reopen (see [`Stores::forget`]) only drops the cache's
//! handle; a request already holding the old [`CatalogDb`] finishes on it,
//! and its pool closes when the last such handle is dropped.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

use super::{CatalogDb, StoreName};
use crate::error::OkfError;
use crate::store::stores::{self, CredentialKind, StoreLocation, StoreRow};
use crate::telemetry::subscriber::split_event;

/// The `search_path` of every store pool. Until the catalog tables move
/// into their own schema (Task 11) they are in `public`; that task changes
/// this one constant to `catalog`.
pub const STORE_SEARCH_PATH: &str = "public";

/// The most stores the registry keeps open at once: a bound on memory for a
/// registry that is meant to hold a handful.
const MAX_OPEN_STORES: u64 = 1024;

/// What the pools of a registry are built with: the `store_*` settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSettings {
    /// The most connections one store's pool opens (`store_max_connections`).
    pub max_connections: u32,
    /// How long an idle connection is kept (`store_idle_timeout`).
    pub idle_timeout: Duration,
    /// How long opening a pool, or waiting for one of its connections, may
    /// take (`store_connect_timeout`).
    pub connect_timeout: Duration,
}

impl PoolSettings {
    /// The settings in `config`.
    ///
    /// # Arguments
    /// * `config` - the server's settings.
    ///
    /// # Returns
    /// The three `store_*` values.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "copies three values")
    )]
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            max_connections: config.store_max_connections,
            idle_timeout: config.store_idle_timeout,
            connect_timeout: config.store_connect_timeout,
        }
    }
}

/// Why a pool could not be opened. Shared (behind an `Arc`) by the callers
/// that waited on one open, so it holds no sqlx error, which cannot be
/// cloned; the detail goes to the local log where it happens.
#[derive(Debug)]
enum OpenFailure {
    /// There is no `stores` row of that name.
    NoSuchStore,
    /// The account database could not answer for the row.
    Registry,
    /// The store's database could not be reached or its credential found.
    Unreachable,
}

/// Reads one environment variable: `None` when it is unset.
type Environment = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Every store's pool, opened on first use.
///
/// Cloning is cheap: the clones share one set of pools.
#[derive(Clone)]
pub struct Stores {
    inner: Arc<Inner>,
}

struct Inner {
    // The account database: where the `stores` rows are, and the connection
    // settings a store inside it (or a host store's user) inherits.
    accounts: PgPool,
    settings: PoolSettings,
    pools: moka::future::Cache<StoreName, CatalogDb>,
    // Reads an environment variable (an `env` credential's password).
    // Injected so tests need not change the process's environment.
    environment: Environment,
    // Moved by every `forget`. An open that overlapped a forget may have
    // read the row before the change, so it serves its caller but is not
    // kept.
    forgets: AtomicU64,
    // How many pools were opened, for tests.
    #[cfg(test)]
    opened: AtomicU64,
    // Tests: makes the next open stop once it has read the row.
    #[cfg(test)]
    gate: std::sync::Mutex<Option<OpenGate>>,
}

/// A test's hold on an open: the open says when it has read the row, then
/// waits to be let go.
#[cfg(test)]
struct OpenGate {
    reached: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl std::fmt::Debug for Stores {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "a Debug impl")
    )]
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Stores")
            .field("open", &self.inner.pools.entry_count())
            .finish_non_exhaustive()
    }
}

impl Stores {
    /// An empty registry: no pool is opened until a store is asked for.
    ///
    /// # Arguments
    /// * `accounts` - a pool on the account database, which holds the
    ///   `stores` rows and whose connection settings (user, TLS) a store
    ///   inherits.
    /// * `settings` - the size and timeouts of every store pool.
    ///
    /// # Returns
    /// The registry.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "builds an empty cache, no I/O")
    )]
    pub fn new(accounts: PgPool, settings: PoolSettings) -> Self {
        Self::with_environment(
            accounts,
            settings,
            Arc::new(|name| std::env::var(name).ok()),
        )
    }

    /// [`Stores::new`] with the environment given: tests pass a map, so no
    /// test changes the process's own variables.
    ///
    /// # Arguments
    /// * `accounts`, `settings` - as for [`Stores::new`].
    /// * `environment` - reads a variable by name.
    ///
    /// # Returns
    /// The registry.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "builds an empty cache, no I/O")
    )]
    fn with_environment(
        accounts: PgPool,
        settings: PoolSettings,
        environment: Environment,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                accounts,
                settings,
                environment,
                pools: moka::future::Cache::builder()
                    .max_capacity(MAX_OPEN_STORES)
                    .build(),
                forgets: AtomicU64::new(0),
                #[cfg(test)]
                opened: AtomicU64::new(0),
                #[cfg(test)]
                gate: std::sync::Mutex::new(None),
            }),
        }
    }

    /// The store's database, opening its pool on first use.
    ///
    /// Callers that ask for one store together share one open. A failed
    /// open is not remembered: the next call tries again.
    ///
    /// # Arguments
    /// * `store` - the store's name.
    ///
    /// # Returns
    /// The store's handle (cheap to clone).
    ///
    /// # Errors
    /// [`OkfError::NotFound`] for a store with no row;
    /// [`OkfError::StoreUnavailable`] if its pool cannot connect within
    /// `store_connect_timeout` (the reply never names the store or its
    /// host); [`OkfError::Busy`] if the account database could not be read
    /// for the row.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn get(&self, store: &StoreName) -> Result<CatalogDb, OkfError> {
        // Set by the initialiser, which borrows it: only a call that ran
        // the open checks for an overtaking `forget` below.
        let ran = AtomicBool::new(false);
        let forgets_before = self.inner.forgets.load(Ordering::SeqCst);
        let result = self
            .inner
            .pools
            .try_get_with(store.clone(), async {
                ran.store(true, Ordering::SeqCst);
                // Boxed: the open's state is large and `get` sits inside
                // deep request futures, where a debug build's stack is small.
                Box::pin(self.open(store)).await
            })
            .await;
        // An open that a `forget` overtook is returned but not kept, so the
        // next call reads the new row.
        if ran.load(Ordering::SeqCst) && self.inner.forgets.load(Ordering::SeqCst) != forgets_before
        {
            self.inner.pools.invalidate(store).await;
        }
        result.map_err(|failure| match &*failure {
            OpenFailure::NoSuchStore => OkfError::NotFound {
                path: "store".to_string(),
            },
            OpenFailure::Registry => OkfError::Busy {
                reason: "the store registry could not be read".to_string(),
            },
            OpenFailure::Unreachable => OkfError::store_unavailable(store),
        })
    }

    /// Every store's name, from the `stores` table (for fan-out to all
    /// stores: housekeeping, audit). Opens no pool.
    ///
    /// # Returns
    /// The names, in name order.
    ///
    /// # Errors
    /// [`OkfError::Database`].
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn all(&self) -> Result<Vec<StoreName>, OkfError> {
        stores::names(&self.inner.accounts).await
    }

    /// Drops a store's pool, so the next [`Stores::get`] reads the row again
    /// and reconnects (called when a `stores` row changes). A request that
    /// holds the old handle finishes on it.
    ///
    /// # Arguments
    /// * `store` - the store's name.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn forget(&self, store: &StoreName) {
        self.inner.forgets.fetch_add(1, Ordering::SeqCst);
        self.inner.pools.invalidate(store).await;
    }

    /// Drops every store's pool: what a replica does when it may have missed
    /// `stores` changes (the change feed reconnected).
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn forget_all(&self) {
        self.inner.forgets.fetch_add(1, Ordering::SeqCst);
        self.inner.pools.invalidate_all();
        // Settle moka's bookkeeping now, so the next `get` misses.
        self.inner.pools.run_pending_tasks().await;
    }

    /// How many pools this registry has opened (tests: one per store at
    /// most, however many callers asked together).
    #[cfg(test)]
    pub(crate) fn pools_opened(&self) -> u64 {
        self.inner.opened.load(Ordering::SeqCst)
    }

    /// Tests: if a gate is set, takes it, says the row has been read, and
    /// waits to be released. The next open is not held.
    #[cfg(test)]
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "a test hook")
    )]
    async fn pause_if_gated(&self) {
        let gate = self.inner.gate.lock().expect("the gate lock").take();
        if let Some(gate) = gate {
            // `notify_one` keeps a permit, so it does not matter whether the
            // test is already waiting.
            gate.reached.notify_one();
            gate.release.notified().await;
        }
    }

    /// Reads the store's row and opens its pool.
    ///
    /// # Errors
    /// An [`OpenFailure`]; the detail is logged here, on the local side
    /// only (a host name is operator detail).
    #[tracing::instrument(level = "debug", skip_all)]
    async fn open(&self, store: &StoreName) -> Result<CatalogDb, OpenFailure> {
        let row = match stores::by_name(&self.inner.accounts, store).await {
            Ok(Some(row)) => row,
            Ok(None) => return Err(OpenFailure::NoSuchStore),
            Err(error) => {
                split_event!(
                    warn,
                    { kind = error.kind(), },
                    { %error, },
                    "the store registry could not be read"
                );
                return Err(OpenFailure::Registry);
            }
        };
        #[cfg(test)]
        self.pause_if_gated().await;
        let options = self.connect_options(&row).map_err(|reason| {
            split_event!(
                warn,
                {},
                { store = %store, reason, },
                "a store's connection settings are unusable"
            );
            OpenFailure::Unreachable
        })?;
        let settings = self.inner.settings;
        let pool_options = PgPoolOptions::new()
            .max_connections(settings.max_connections)
            // A store that is not used holds no connection.
            .min_connections(0)
            .idle_timeout(Some(settings.idle_timeout))
            .acquire_timeout(settings.connect_timeout)
            // `Box::pin(async move { .. })` is the shape sqlx wants for an
            // async callback; it runs once per new physical connection.
            .after_connect(|connection, _meta| {
                Box::pin(async move {
                    sqlx::query("SELECT pg_catalog.set_config('search_path', $1, false)")
                        .bind(STORE_SEARCH_PATH)
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            });
        // `connect_with` opens one connection now, so an unreachable store
        // fails here and not on the first query. The pool's own acquire
        // timeout already bounds it; the outer timeout is a belt for a
        // resolver or TLS handshake that ignores it.
        let connecting = pool_options.connect_with(options);
        let pool = match tokio::time::timeout(settings.connect_timeout, connecting).await {
            Ok(Ok(pool)) => pool,
            Ok(Err(error)) => {
                split_event!(
                    warn,
                    {},
                    { store = %store, %error, },
                    "a store's pool could not connect"
                );
                return Err(OpenFailure::Unreachable);
            }
            Err(_elapsed) => {
                split_event!(
                    warn,
                    {},
                    { store = %store, },
                    "a store's pool did not connect within store_connect_timeout"
                );
                return Err(OpenFailure::Unreachable);
            }
        };
        #[cfg(test)]
        self.inner.opened.fetch_add(1, Ordering::SeqCst);
        Ok(CatalogDb::new(row.name, pool))
    }

    /// The connection settings of a store's row, built on the account
    /// database's (the user and TLS settings are shared).
    ///
    /// # Errors
    /// Why the settings cannot be built (a message for the local log): the
    /// credential's kind is not supported yet (Task 9), or the variable an
    /// `env` credential names is unset.
    #[tracing::instrument(level = "debug", skip_all)]
    fn connect_options(&self, row: &StoreRow) -> Result<PgConnectOptions, String> {
        let base = (*self.inner.accounts.connect_options()).clone();
        match &row.location {
            StoreLocation::AccountDatabase => Ok(base),
            StoreLocation::Host {
                host,
                port,
                database,
                credential_kind,
                credential_ref,
            } => match credential_kind {
                // The password comes from the named variable only: the
                // account database's password is never sent to another host.
                CredentialKind::Env => {
                    let password = (self.inner.environment)(credential_ref)
                        .ok_or_else(|| format!("the variable {credential_ref} is not set"))?;
                    Ok(base
                        .host(host)
                        .port(*port)
                        .database(database)
                        .password(&password))
                }
                CredentialKind::Iam | CredentialKind::SecretsManager => Err(format!(
                    "credential kind {} is not supported yet",
                    credential_kind.as_str()
                )),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// Settings for tests: a short connect timeout, a small pool.
    fn settings() -> PoolSettings {
        PoolSettings {
            max_connections: 4,
            idle_timeout: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(2),
        }
    }

    /// The password in a `postgres://user:password@host:port` URL.
    fn password_of(url: &str) -> String {
        let after_scheme = url.split("://").nth(1).expect("a URL");
        let credentials = after_scheme.split('@').next().expect("credentials");
        credentials
            .split(':')
            .nth(1)
            .unwrap_or_default()
            .to_string()
    }

    /// Adds a `host` store row for `name` on `127.0.0.1:port`, with an `env`
    /// credential named `SHARED_1_PASSWORD`.
    async fn add_host_store(pool: &PgPool, name: &str, host: &str, port: u16, database: &str) {
        sqlx::query(
            "INSERT INTO stores (name, location_kind, host, port, database, \
             credential_kind, credential_ref, created_at) \
             VALUES ($1, 'host', $2, $3, $4, 'env', 'SHARED_1_PASSWORD', now())",
        )
        .bind(name)
        .bind(host)
        .bind(i32::from(port))
        .bind(database)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn account_database_store_opens_its_own_pool() {
        let db = crate::db::test_db().await;
        let stores = Stores::new(db.pool.clone(), settings());

        let store = stores.get(&StoreName::main()).await.unwrap();
        // Both connections are held at once, so they are two backends.
        let mut held = db.pool.acquire().await.unwrap();
        let mut store_held = store.executor().acquire().await.unwrap();

        assert_eq!(store.store(), &StoreName::main());
        let store_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *store_held)
            .await
            .unwrap();
        let account_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *held)
            .await
            .unwrap();
        assert_ne!(store_pid, account_pid);
        // Its own size, and the store search path, set on every connection.
        assert_eq!(store.executor().options().get_max_connections(), 4);
        assert_ne!(
            store.executor().options().get_max_connections(),
            db.pool.options().get_max_connections()
        );
        let search_path: String = sqlx::query_scalar("SHOW search_path")
            .fetch_one(store.executor())
            .await
            .unwrap();
        assert_eq!(search_path, STORE_SEARCH_PATH);
        assert_eq!(stores.pools_opened(), 1);
        assert_eq!(stores.all().await.unwrap(), [StoreName::main()]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_exhausted_open_pool_is_store_unavailable_without_naming_the_store() {
        let db = crate::db::test_db().await;
        let stores = Stores::with_environment(
            db.pool.clone(),
            PoolSettings {
                max_connections: 2,
                idle_timeout: Duration::from_secs(60),
                connect_timeout: Duration::from_millis(300),
            },
            Arc::new(|_| None),
        );
        let store = stores.get(&StoreName::main()).await.unwrap();
        // Every connection of the open pool is in use.
        let _first = store.begin().await.unwrap();
        let _second = store.begin().await.unwrap();

        let started = Instant::now();
        let failed = store.begin().await.unwrap_err();

        assert!(
            matches!(failed, OkfError::StoreUnavailable { .. }),
            "{failed:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!failed.to_json().to_string().contains("main"));
        // The pool is still the cached one: nothing reopened.
        assert_eq!(stores.pools_opened(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_first_use_opens_one_pool() {
        let db = crate::db::test_db().await;
        let stores = Stores::new(db.pool.clone(), settings());

        let mut asks = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let stores = stores.clone();
            asks.spawn(async move { stores.get(&StoreName::main()).await.map(|_| ()) });
        }
        while let Some(done) = asks.join_next().await {
            done.unwrap().unwrap();
        }

        assert_eq!(stores.pools_opened(), 1);
        // A later call is a hit.
        stores.get(&StoreName::main()).await.unwrap();
        assert_eq!(stores.pools_opened(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unreachable_store_is_store_unavailable_within_the_timeout() {
        let db = crate::db::test_db().await;
        // A port nothing listens on: bind one, then close it.
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        add_host_store(&db.pool, "shared-1", "127.0.0.1", closed, "okf").await;
        let password = password_of(&db.url);
        let stores = Stores::with_environment(
            db.pool.clone(),
            settings(),
            Arc::new(move |name| (name == "SHARED_1_PASSWORD").then(|| password.clone())),
        );
        let name = StoreName::parse("shared-1").unwrap();

        let started = Instant::now();
        // The outer bound only keeps a regression from hanging the suite.
        let failed = tokio::time::timeout(Duration::from_secs(60), stores.get(&name))
            .await
            .expect("the open ends instead of hanging")
            .unwrap_err();
        let elapsed = started.elapsed();

        assert!(
            matches!(failed, OkfError::StoreUnavailable { .. }),
            "{failed:?}"
        );
        assert!(
            elapsed < settings().connect_timeout + Duration::from_secs(10),
            "took {elapsed:?}"
        );
        // The reply names neither the store nor its host.
        let reply = failed.to_json().to_string();
        for private in ["shared-1", "127.0.0.1", &closed.to_string()] {
            assert!(!reply.contains(private), "{private} leaked into {reply}");
        }
        assert_eq!(stores.pools_opened(), 0);

        // A failed open is not remembered: once the row points at the
        // running server, the next call connects.
        let host = db.pool.connect_options().get_host().to_string();
        let port = db.pool.connect_options().get_port();
        let database = db
            .pool
            .connect_options()
            .get_database()
            .unwrap()
            .to_string();
        sqlx::query(
            "UPDATE stores SET host = $2, port = $3, database = $4 WHERE name = 'shared-1'",
        )
        .bind("shared-1")
        .bind(host)
        .bind(i32::from(port))
        .bind(database)
        .execute(&db.pool)
        .await
        .unwrap();
        let store = stores.get(&name).await.unwrap();
        let one: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(store.executor())
            .await
            .unwrap();
        assert_eq!(one, 1);
    }

    /// Starts a `get` that stops after reading its row, runs a change to the
    /// row and a forget (or a forget of everything), lets the open finish, and checks
    /// that the pool it built served its caller but was not kept.
    async fn an_open_overtaken_by(forget_everything: bool) {
        let db = crate::db::test_db().await;
        let stores = Stores::new(db.pool.clone(), settings());
        let reached = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        *stores.inner.gate.lock().unwrap() = Some(OpenGate {
            reached: Arc::clone(&reached),
            release: Arc::clone(&release),
        });
        let overlapping = {
            let stores = stores.clone();
            tokio::spawn(async move { stores.get(&StoreName::main()).await })
        };
        reached.notified().await;

        // The row changes and replicas are told, while the open holds the
        // old row.
        sqlx::query("UPDATE stores SET created_at = now() WHERE name = 'main'")
            .execute(&db.pool)
            .await
            .unwrap();
        if forget_everything {
            stores.forget_all().await;
        } else {
            stores.forget(&StoreName::main()).await;
        }
        release.notify_one();

        // The overlapping caller still gets its pool, and it works.
        let served = overlapping.await.unwrap().unwrap();
        let one: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(served.executor())
            .await
            .unwrap();
        assert_eq!(one, 1);
        assert_eq!(stores.pools_opened(), 1);
        // The next call reads the new row and opens afresh instead of
        // keeping the pool built from the old one.
        stores.get(&StoreName::main()).await.unwrap();
        assert_eq!(stores.pools_opened(), 2);
        // That one is kept.
        stores.get(&StoreName::main()).await.unwrap();
        assert_eq!(stores.pools_opened(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_open_overtaken_by_a_forget_is_served_but_not_kept() {
        an_open_overtaken_by(false).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_open_overtaken_by_a_forget_all_is_served_but_not_kept() {
        an_open_overtaken_by(true).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_store_with_no_row_is_not_found_and_an_unset_credential_is_unavailable() {
        let db = crate::db::test_db().await;
        add_host_store(&db.pool, "shared-1", "db.example.internal", 5432, "okf").await;
        let stores = Stores::with_environment(db.pool.clone(), settings(), Arc::new(|_| None));

        let unknown = stores
            .get(&StoreName::parse("acme").unwrap())
            .await
            .unwrap_err();
        let unset = stores
            .get(&StoreName::parse("shared-1").unwrap())
            .await
            .unwrap_err();

        assert!(matches!(unknown, OkfError::NotFound { .. }), "{unknown:?}");
        assert!(
            matches!(unset, OkfError::StoreUnavailable { .. }),
            "{unset:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgetting_a_store_lets_a_user_of_the_old_pool_finish() {
        let db = crate::db::test_db().await;
        let stores = Stores::new(db.pool.clone(), settings());
        let old = stores.get(&StoreName::main()).await.unwrap();
        // A transaction in flight on the old pool.
        let mut tx = old.begin().await.unwrap();

        stores.forget(&StoreName::main()).await;
        let new = stores.get(&StoreName::main()).await.unwrap();

        assert_eq!(stores.pools_opened(), 2);
        let one: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(one, 1);
        tx.commit().await.unwrap();
        let two: i32 = sqlx::query_scalar("SELECT 2")
            .fetch_one(new.executor())
            .await
            .unwrap();
        assert_eq!(two, 2);
        stores.forget_all().await;
        stores.get(&StoreName::main()).await.unwrap();
        assert_eq!(stores.pools_opened(), 3);
    }
}

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

#[cfg(test)]
use super::credentials::Environment;
use super::credentials::{
    is_auth_failure, spawn_keeper, Credential, CredentialSource, Keeper, Providers, Refresh,
};
use super::{CatalogDb, StoreName};
use crate::error::OkfError;
use crate::store::stores::{self, StoreLocation, StoreRow};
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

/// What the registry keeps for an open store: its handle, and the task that
/// keeps its credential current (if the credential ever changes). Dropping
/// the entry (on [`Stores::forget`], [`Stores::forget_all`] or when the
/// registry goes) ends that task, so a forgotten store leaves none behind.
struct OpenStore {
    db: CatalogDb,
    // Only held, for its `Drop`.
    _keeper: Option<Keeper>,
}

/// Why one attempt to connect a pool failed (logged where it happened).
enum ConnectFailure {
    /// The server refused the login: a wrong or expired password or token.
    LoginRefused,
    /// Anything else, including the timeout.
    Other,
}

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
    pools: moka::future::Cache<StoreName, Arc<OpenStore>>,
    // How a row's credential is found: the environment and the AWS clients,
    // injected so tests need not change the process's environment or call AWS.
    providers: Providers,
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
        Self::with_providers(
            accounts,
            settings,
            Providers::aws(Arc::new(|name| std::env::var(name).ok())),
        )
    }

    /// [`Stores::new`] with the credential providers given: tests pass a
    /// map for the environment and mocks for the AWS clients.
    ///
    /// # Arguments
    /// * `accounts`, `settings` - as for [`Stores::new`].
    /// * `providers` - how a row's credential is found.
    ///
    /// # Returns
    /// The registry.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "builds an empty cache, no I/O")
    )]
    fn with_providers(accounts: PgPool, settings: PoolSettings, providers: Providers) -> Self {
        Self {
            inner: Arc::new(Inner {
                accounts,
                settings,
                providers,
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
        let result = result.map(|open| open.db.clone());
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
        // Settle moka's bookkeeping now, so the entry (and with it the
        // store's credential task) is dropped before this returns.
        self.inner.pools.run_pending_tasks().await;
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
    async fn open(&self, store: &StoreName) -> Result<Arc<OpenStore>, OpenFailure> {
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
        self.open_row(row).await
    }

    /// Opens a pool from a row, which need not be stored yet: the part of
    /// [`Stores::open`] after the row is read, shared with [`Stores::check`].
    ///
    /// # Errors
    /// An [`OpenFailure`]; the detail is logged here, on the local side only.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn open_row(&self, row: StoreRow) -> Result<Arc<OpenStore>, OpenFailure> {
        let store = &row.name;
        let (base, source) = self.connection_of(&row);
        let pool = self
            .connect_with_credential(store, &base, source.as_deref())
            .await?;
        #[cfg(test)]
        self.inner.opened.fetch_add(1, Ordering::SeqCst);
        // A credential that changes is kept current for as long as this
        // entry lives.
        let keeper = source.and_then(|source| spawn_keeper(source, base, pool.clone()));
        let db = CatalogDb::new(row.name, pool);
        Ok(Arc::new(OpenStore {
            db,
            _keeper: keeper,
        }))
    }

    /// The connection settings of a store's row, without its login, built
    /// on the account database's (the user and TLS settings are shared),
    /// and where the login comes from.
    ///
    /// # Returns
    /// The options, and the credential source: `None` for a store inside
    /// the account database, which uses the account pool's own login.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "builds options, no I/O")
    )]
    fn connection_of(
        &self,
        row: &StoreRow,
    ) -> (PgConnectOptions, Option<Arc<dyn CredentialSource>>) {
        let base = (*self.inner.accounts.connect_options()).clone();
        match &row.location {
            StoreLocation::AccountDatabase => (base, None),
            StoreLocation::Host {
                host,
                port,
                database,
                credential_kind,
                credential_ref,
            } => (
                base.host(host).port(*port).database(database),
                Some(
                    self.inner
                        .providers
                        .source(*credential_kind, credential_ref, host, *port),
                ),
            ),
        }
    }

    /// Fetches the login and connects the pool. A refused login is fetched
    /// again and tried once more, for a credential that changes (a rotated
    /// secret, an expired token); a credential that never changes (`env`)
    /// is not.
    ///
    /// # Errors
    /// [`OpenFailure::Unreachable`] for any failure; the detail is logged
    /// here, and never holds the login.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn connect_with_credential(
        &self,
        store: &StoreName,
        base: &PgConnectOptions,
        source: Option<&dyn CredentialSource>,
    ) -> Result<PgPool, OpenFailure> {
        let mut attempts_left = match source.map(CredentialSource::refresh) {
            None | Some(Refresh::Never) => 1,
            Some(Refresh::OnAuthFailure | Refresh::Every(_)) => 2,
        };
        loop {
            let credential = match source {
                Some(source) => Some(source.fetch().await.map_err(|error| {
                    split_event!(
                        warn,
                        {},
                        { store = %store, %error, },
                        "a store's credential could not be found"
                    );
                    OpenFailure::Unreachable
                })?),
                None => None,
            };
            attempts_left -= 1;
            match self.connect_pool(store, base, credential).await {
                Ok(pool) => return Ok(pool),
                Err(ConnectFailure::LoginRefused) if attempts_left > 0 => {}
                Err(_) => return Err(OpenFailure::Unreachable),
            }
        }
    }

    /// Opens one pool, with the login applied to `base`.
    ///
    /// # Errors
    /// Why it failed; logged here.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn connect_pool(
        &self,
        store: &StoreName,
        base: &PgConnectOptions,
        credential: Option<Credential>,
    ) -> Result<PgPool, ConnectFailure> {
        let options = match credential {
            Some(credential) => credential.apply(base.clone()),
            None => base.clone(),
        };
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
        match tokio::time::timeout(settings.connect_timeout, connecting).await {
            Ok(Ok(pool)) => Ok(pool),
            Ok(Err(error)) => {
                split_event!(
                    warn,
                    {},
                    { store = %store, %error, },
                    "a store's pool could not connect"
                );
                Err(if is_auth_failure(&error) {
                    ConnectFailure::LoginRefused
                } else {
                    ConnectFailure::Other
                })
            }
            Err(_elapsed) => {
                split_event!(
                    warn,
                    {},
                    { store = %store, },
                    "a store's pool did not connect within store_connect_timeout"
                );
                Err(ConnectFailure::Other)
            }
        }
    }

    /// Whether a store that has no row yet could be opened: the same open
    /// path as [`Stores::get`] (connection settings, credential, timeout),
    /// on a row the caller has not stored. The pool is closed again, its
    /// credential task ends, and nothing is cached.
    ///
    /// # Arguments
    /// * `row` - the row about to be stored.
    ///
    /// # Returns
    /// `Ok(())` if a connection was made.
    ///
    /// # Errors
    /// [`OkfError::StoreUnavailable`] if it cannot connect within
    /// `store_connect_timeout` (the error names no host and no credential;
    /// the cause is logged on the local side only).
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn check(&self, row: StoreRow) -> Result<(), OkfError> {
        let name = row.name.clone();
        // Boxed for the same reason as in `get`: the open's state is large.
        let open = Box::pin(self.open_row(row))
            .await
            .map_err(|_failure| OkfError::store_unavailable(&name))?;
        open.db.executor().close().await;
        Ok(())
    }

    /// [`Stores::new`] with the environment given and the real AWS clients
    /// (which the tests using this never reach).
    #[cfg(test)]
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "a test constructor")
    )]
    pub(crate) fn with_environment(
        accounts: PgPool,
        settings: PoolSettings,
        environment: Environment,
    ) -> Self {
        Self::with_providers(accounts, settings, Providers::aws(environment))
    }

    /// Tests: the refresh count of an open store's credential task, which
    /// closes when the task ends. `None` for a store that is not open or
    /// whose credential never changes.
    #[cfg(test)]
    pub(crate) async fn credential_task(
        &self,
        store: &StoreName,
    ) -> Option<tokio::sync::watch::Receiver<u64>> {
        let open = self.inner.pools.get(store).await?;
        open._keeper.as_ref().map(Keeper::watch)
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

    // -- credential kinds (issue #967) --------------------------------------

    use std::sync::atomic::AtomicUsize;

    use async_trait::async_trait;
    use sqlx::{AssertSqlSafe, Connection};

    use super::super::credentials::{
        CredentialError, Providers, SecretReader, TokenSigner, IAM_REFRESH_EVERY,
    };

    /// The `iam` signer: a token is `token-N`, N counting 15-minute windows
    /// of a test-controlled clock, so it "expires" when the test moves the
    /// clock and nothing sleeps. `stale_first` makes the first answer one
    /// the database does not accept.
    struct ClockSigner {
        now: Arc<AtomicU64>,
        calls: AtomicUsize,
        stale_first: bool,
        always_wrong: bool,
        token_override: std::sync::Mutex<Option<String>>,
        seen: std::sync::Mutex<Vec<(String, u16, String)>>,
    }

    impl ClockSigner {
        fn plain(now: &Arc<AtomicU64>) -> Self {
            Self {
                now: Arc::clone(now),
                calls: AtomicUsize::new(0),
                stale_first: false,
                always_wrong: false,
                token_override: std::sync::Mutex::new(None),
                seen: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn new(now: &Arc<AtomicU64>) -> Arc<Self> {
            Arc::new(Self::plain(now))
        }

        fn token_now(now: &AtomicU64) -> String {
            format!("token-{}", now.load(Ordering::SeqCst) / 900)
        }
    }

    #[async_trait]
    impl TokenSigner for ClockSigner {
        async fn token(
            &self,
            host: &str,
            port: u16,
            user: &str,
        ) -> Result<String, CredentialError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen
                .lock()
                .unwrap()
                .push((host.to_string(), port, user.to_string()));
            if let Some(token) = &*self.token_override.lock().unwrap() {
                return Ok(token.clone());
            }
            if self.always_wrong || (self.stale_first && call == 0) {
                return Ok("token-stale".to_string());
            }
            Ok(Self::token_now(&self.now))
        }
    }

    /// A secrets manager holding one answer, which a test can replace.
    struct MockSecrets {
        answer: std::sync::Mutex<Result<String, CredentialError>>,
        calls: AtomicUsize,
    }

    impl MockSecrets {
        fn holding(text: &str) -> Arc<Self> {
            Arc::new(Self {
                answer: std::sync::Mutex::new(Ok(text.to_string())),
                calls: AtomicUsize::new(0),
            })
        }

        fn set(&self, answer: Result<String, CredentialError>) {
            *self.answer.lock().unwrap() = answer;
        }
    }

    #[async_trait]
    impl SecretReader for MockSecrets {
        async fn read(&self, _arn: &str) -> Result<String, CredentialError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &*self.answer.lock().unwrap() {
                Ok(text) => Ok(text.clone()),
                Err(CredentialError::Service { service, code }) => Err(CredentialError::Service {
                    service,
                    code: code.clone(),
                }),
                Err(_) => Err(CredentialError::Malformed { what: "the mock" }),
            }
        }
    }

    const EXAMPLE_ARN: &str = "arn:aws:secretsmanager:us-east-1:123456789012:secret:example";

    /// The test server's host, port and database, as a `stores` row spells
    /// them.
    fn server_of(pool: &PgPool) -> (String, u16, String) {
        let options = pool.connect_options();
        (
            options.get_host().to_string(),
            options.get_port(),
            options.get_database().expect("a database").to_string(),
        )
    }

    /// Adds a `host` row for the test server with the given credential.
    async fn add_row(pool: &PgPool, name: &str, kind: &str, reference: &str) {
        let (_, _, database) = server_of(pool);
        add_row_on(pool, name, kind, reference, &database).await;
    }

    /// [`add_row`], naming the database too (the table allows one row per
    /// host, port and database).
    async fn add_row_on(pool: &PgPool, name: &str, kind: &str, reference: &str, database: &str) {
        let (host, port, _) = server_of(pool);
        sqlx::query(
            "INSERT INTO stores (name, location_kind, host, port, database, \
             credential_kind, credential_ref, created_at) \
             VALUES ($1, 'host', $2, $3, $4, $5, $6, now())",
        )
        .bind(name)
        .bind(host)
        .bind(i32::from(port))
        .bind(database)
        .bind(kind)
        .bind(reference)
        .execute(pool)
        .await
        .unwrap();
    }

    /// `name` as a quoted SQL identifier (a `"` inside doubles).
    fn quoted(name: &str) -> String {
        format!("\"{}\"", name.replace('"', "\"\""))
    }

    /// A login role of its own (roles belong to the whole server, so the
    /// name carries the test database's).
    async fn make_role(pool: &PgPool, password: &str) -> String {
        let (_, _, database) = server_of(pool);
        make_named_role(pool, &format!("r_{database}"), password).await
    }

    /// A login role called `role`, which may contain a quotation mark.
    async fn make_named_role(pool: &PgPool, role: &str, password: &str) -> String {
        sqlx::query(AssertSqlSafe(format!(
            "CREATE ROLE {} LOGIN PASSWORD '{password}'",
            quoted(role)
        )))
        .execute(pool)
        .await
        .unwrap();
        role.to_string()
    }

    async fn set_password(pool: &PgPool, role: &str, password: &str) {
        sqlx::query(AssertSqlSafe(format!(
            "ALTER ROLE {} PASSWORD '{password}'",
            quoted(role)
        )))
        .execute(pool)
        .await
        .unwrap();
    }

    async fn drop_role(pool: &PgPool, role: &str) {
        sqlx::query(AssertSqlSafe(format!("DROP ROLE {}", quoted(role))))
            .execute(pool)
            .await
            .unwrap();
    }

    fn no_secrets() -> Arc<MockSecrets> {
        MockSecrets::holding("")
    }

    fn iam_stores(
        db: &crate::db::pg_support::TestDb,
        signer: &Arc<ClockSigner>,
        refresh_every: Duration,
    ) -> Stores {
        Stores::with_providers(
            db.pool.clone(),
            settings(),
            Providers::new(
                Arc::new(|_| None),
                no_secrets(),
                Arc::clone(signer) as Arc<dyn TokenSigner>,
            )
            .with_iam_refresh_every(refresh_every),
        )
    }

    async fn current_user(store: &CatalogDb, connection: &mut sqlx::PgConnection) -> String {
        let _ = store;
        sqlx::query_scalar("SELECT current_user::text")
            .fetch_one(connection)
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn iam_replacement_connection_after_the_first_token_expires_uses_a_fresh_token() {
        let db = crate::db::test_db().await;
        let role = make_role(&db.pool, "token-0").await;
        let now = Arc::new(AtomicU64::new(0));
        let signer = ClockSigner::new(&now);
        // A short timer, so the test waits on the refresh count and not on
        // a clock.
        let stores = iam_stores(&db, &signer, Duration::from_millis(20));
        add_row(&db.pool, "shared-1", "iam", &role).await;
        let name = StoreName::parse("shared-1").unwrap();

        let store = stores.get(&name).await.unwrap();
        let mut first = store.executor().acquire().await.unwrap();
        assert_eq!(current_user(&store, &mut first).await, role);
        // The signer was asked for this server and the row's user.
        let (host, port, _) = server_of(&db.pool);
        assert_eq!(signer.seen.lock().unwrap()[0], (host, port, role.clone()));

        // The token expires: the clock passes its window, and the database
        // now wants the next one.
        now.store(900, Ordering::SeqCst);
        set_password(&db.pool, &role, "token-1").await;
        let mut refreshes = stores
            .credential_task(&name)
            .await
            .expect("a credential task");
        let seen = *refreshes.borrow();
        // The second refresh after this point began after the clock moved.
        refreshes
            .wait_for(|count| *count >= seen + 2)
            .await
            .unwrap();

        // The first connection is still held, so this is a new physical
        // connection: it must carry the fresh token.
        let mut second = store.executor().acquire().await.unwrap();
        assert_eq!(current_user(&store, &mut second).await, role);
        // The database really did refuse the old token.
        let (host, port, database) = server_of(&db.pool);
        let stale = db
            .pool
            .connect_options()
            .as_ref()
            .clone()
            .host(&host)
            .port(port)
            .database(&database)
            .username(&role)
            .password("token-0");
        let refused = sqlx::PgConnection::connect_with(&stale)
            .await
            .expect_err("refused");
        assert!(
            crate::stores::credentials::is_auth_failure(&refused),
            "{refused:?}"
        );

        drop((first, second));
        stores.forget_all().await;
        drop_role(&db.pool, &role).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn iam_auth_failure_refreshes_once_and_retries() {
        let db = crate::db::test_db().await;
        let role = make_role(&db.pool, "token-0").await;
        let now = Arc::new(AtomicU64::new(0));
        // The first token the signer gives is one the database refuses.
        let signer = Arc::new(ClockSigner {
            stale_first: true,
            ..ClockSigner::plain(&now)
        });
        let stores = iam_stores(&db, &signer, IAM_REFRESH_EVERY);
        add_row(&db.pool, "shared-1", "iam", &role).await;

        let store = stores
            .get(&StoreName::parse("shared-1").unwrap())
            .await
            .unwrap();

        // One refresh, one retry: two signatures, and the pool works.
        assert_eq!(signer.calls.load(Ordering::SeqCst), 2);
        let one: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(store.executor())
            .await
            .unwrap();
        assert_eq!(one, 1);
        assert_eq!(stores.pools_opened(), 1);

        stores.forget_all().await;
        drop_role(&db.pool, &role).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_login_that_stays_refused_is_tried_twice_and_no_more() {
        let db = crate::db::test_db().await;
        let role = make_role(&db.pool, "token-0").await;
        let now = Arc::new(AtomicU64::new(0));
        let signer = Arc::new(ClockSigner {
            always_wrong: true,
            ..ClockSigner::plain(&now)
        });
        let stores = iam_stores(&db, &signer, IAM_REFRESH_EVERY);
        add_row(&db.pool, "shared-1", "iam", &role).await;
        // An `env` credential does not change, so it is tried once.
        add_row_on(&db.pool, "shared-2", "env", "WRONG_PASSWORD", "other").await;
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&reads);
        let env_stores = Stores::with_environment(
            db.pool.clone(),
            settings(),
            Arc::new(move |_| {
                counted.fetch_add(1, Ordering::SeqCst);
                Some("not-the-password".to_string())
            }),
        );

        let failed = stores
            .get(&StoreName::parse("shared-1").unwrap())
            .await
            .unwrap_err();
        let env_failed = env_stores
            .get(&StoreName::parse("shared-2").unwrap())
            .await
            .unwrap_err();

        assert!(
            matches!(failed, OkfError::StoreUnavailable { .. }),
            "{failed:?}"
        );
        assert!(
            matches!(env_failed, OkfError::StoreUnavailable { .. }),
            "{env_failed:?}"
        );
        assert_eq!(signer.calls.load(Ordering::SeqCst), 2);
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert_eq!(stores.pools_opened(), 0);
        drop_role(&db.pool, &role).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_login_on_an_open_pool_fetches_a_fresh_one_for_the_next_connection() {
        let db = crate::db::test_db().await;
        let role = make_role(&db.pool, "token-0").await;
        let now = Arc::new(AtomicU64::new(0));
        let signer = ClockSigner::new(&now);
        // The timer is the default 10 minutes: only a refused login can
        // cause the refresh this test waits for.
        let stores = iam_stores(&db, &signer, IAM_REFRESH_EVERY);
        add_row(&db.pool, "shared-1", "iam", &role).await;
        let name = StoreName::parse("shared-1").unwrap();
        let store = stores.get(&name).await.unwrap();
        let _held = store.executor().acquire().await.unwrap();
        let mut refreshes = stores.credential_task(&name).await.unwrap();

        now.store(900, Ordering::SeqCst);
        set_password(&db.pool, &role, "token-1").await;
        // A new physical connection is refused: the caller sees an
        // unavailable store, not a database error.
        let refused = store.begin().await.unwrap_err();
        assert!(
            matches!(refused, OkfError::StoreUnavailable { .. }),
            "{refused:?}"
        );
        refreshes.wait_for(|count| *count >= 1).await.unwrap();

        let mut tx = store.begin().await.unwrap();
        let user: String = sqlx::query_scalar("SELECT current_user::text")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(user, role);
        // The refused attempt and nothing else caused the second signature.
        assert_eq!(signer.calls.load(Ordering::SeqCst), 2);

        drop(tx);
        stores.forget_all().await;
        drop_role(&db.pool, &role).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_secrets_manager_store_opens_and_follows_a_rotated_secret() {
        let db = crate::db::test_db().await;
        let role = make_role(&db.pool, "pw-1").await;
        let secrets = MockSecrets::holding(&format!(
            r#"{{"username":"{role}","password":"pw-1","engine":"postgres"}}"#
        ));
        let stores = Stores::with_providers(
            db.pool.clone(),
            settings(),
            Providers::new(
                Arc::new(|_| None),
                Arc::clone(&secrets) as Arc<dyn SecretReader>,
                Arc::new(ClockSigner::plain(&Arc::new(AtomicU64::new(0)))),
            ),
        );
        add_row(&db.pool, "shared-1", "secrets-manager", EXAMPLE_ARN).await;
        let name = StoreName::parse("shared-1").unwrap();

        let store = stores.get(&name).await.unwrap();
        let mut first = store.executor().acquire().await.unwrap();
        assert_eq!(current_user(&store, &mut first).await, role);
        assert_eq!(secrets.calls.load(Ordering::SeqCst), 1);

        // The secret is rotated: the database and the secret change.
        set_password(&db.pool, &role, "pw-2").await;
        secrets.set(Ok(format!(r#"{{"username":"{role}","password":"pw-2"}}"#)));
        let mut refreshes = stores.credential_task(&name).await.unwrap();
        let refused = store.begin().await.unwrap_err();
        assert!(
            matches!(refused, OkfError::StoreUnavailable { .. }),
            "{refused:?}"
        );
        refreshes.wait_for(|count| *count >= 1).await.unwrap();
        let mut second = store.executor().acquire().await.unwrap();
        assert_eq!(current_user(&store, &mut second).await, role);
        assert_eq!(secrets.calls.load(Ordering::SeqCst), 2);

        drop((first, second));
        stores.forget_all().await;
        drop_role(&db.pool, &role).await;
    }

    /// A rotated secret, then a plain read on the open pool: the read is
    /// refused, the credential task refreshes, and the next read works.
    /// `quoted_role` gives the role a `"` in its name.
    async fn a_plain_read_after_a_rotation_refreshes(quoted_role: bool) {
        let db = crate::db::test_db().await;
        let role = if quoted_role {
            let (_, _, database) = server_of(&db.pool);
            make_named_role(&db.pool, &format!("app\"{database}"), "pw-1").await
        } else {
            make_role(&db.pool, "pw-1").await
        };
        let secret = |password: &str| {
            serde_json::json!({ "username": role, "password": password }).to_string()
        };
        let secrets = MockSecrets::holding(&secret("pw-1"));
        let stores = Stores::with_providers(
            db.pool.clone(),
            settings(),
            Providers::new(
                Arc::new(|_| None),
                Arc::clone(&secrets) as Arc<dyn SecretReader>,
                Arc::new(ClockSigner::plain(&Arc::new(AtomicU64::new(0)))),
            ),
        );
        add_row(&db.pool, "shared-1", "secrets-manager", EXAMPLE_ARN).await;
        let name = StoreName::parse("shared-1").unwrap();
        let store = stores.get(&name).await.unwrap();
        // One connection stays busy, so the reads below need a new one.
        let _held = store.executor().acquire().await.unwrap();
        let mut refreshes = stores.credential_task(&name).await.unwrap();

        set_password(&db.pool, &role, "pw-2").await;
        secrets.set(Ok(secret("pw-2")));
        // A read that goes straight to the pool and propagates with `?`,
        // never through `CatalogDb::classify` (the shape of the store
        // layer's functions).
        async fn plain_read(db: &CatalogDb) -> Result<i32, OkfError> {
            Ok(sqlx::query_scalar("SELECT 1")
                .fetch_one(db.executor())
                .await?)
        }
        let refused = plain_read(&store).await.unwrap_err();
        assert!(matches!(refused, OkfError::Database { .. }), "{refused:?}");
        refreshes.wait_for(|count| *count >= 1).await.unwrap();

        assert_eq!(plain_read(&store).await.unwrap(), 1);
        assert_eq!(secrets.calls.load(Ordering::SeqCst), 2);

        stores.forget_all().await;
        drop_role(&db.pool, &role).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_plain_read_refused_after_a_rotation_refreshes_the_login_and_the_next_read_works() {
        a_plain_read_after_a_rotation_refreshes(false).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_role_with_a_quotation_mark_still_refreshes_after_a_rotation() {
        a_plain_read_after_a_rotation_refreshes(true).await;
    }

    /// Opens an IAM store, forgets it (one store, or all), and checks that
    /// its credential task ends.
    async fn forgetting_ends_the_credential_task(forget_everything: bool) {
        let db = crate::db::test_db().await;
        let role = make_role(&db.pool, "token-0").await;
        let signer = ClockSigner::new(&Arc::new(AtomicU64::new(0)));
        let stores = iam_stores(&db, &signer, IAM_REFRESH_EVERY);
        add_row(&db.pool, "shared-1", "iam", &role).await;
        let name = StoreName::parse("shared-1").unwrap();
        let _store = stores.get(&name).await.unwrap();
        let mut task = stores.credential_task(&name).await.expect("a task");
        assert!(
            task.has_changed().is_ok(),
            "the task runs while the store is open"
        );

        if forget_everything {
            stores.forget_all().await;
        } else {
            stores.forget(&name).await;
        }

        // The task owns the channel's sender: it closes when the task ends.
        // The outer timeout only keeps a regression from hanging the suite.
        tokio::time::timeout(Duration::from_secs(30), async {
            while task.changed().await.is_ok() {}
        })
        .await
        .expect("the credential task ends when the store is forgotten");
        drop_role(&db.pool, &role).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgetting_a_store_ends_its_credential_task() {
        forgetting_ends_the_credential_task(false).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgetting_every_store_ends_their_credential_tasks() {
        forgetting_ends_the_credential_task(true).await;
    }

    /// A log sink for one test.
    #[derive(Clone, Default)]
    struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Captured;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }

        fn clear(&self) {
            self.0.lock().unwrap().clear();
        }
    }

    // A current-thread runtime, so the thread-local subscriber sees every
    // event the registry and the pool's tasks emit.
    #[tokio::test]
    async fn a_credential_never_appears_in_logs_or_errors() {
        const PASSWORD: &str = "pw-sentinel-aaa";
        const TOKEN: &str = "tok-sentinel-bbb";
        const SECRET_ARN: &str =
            "arn:aws:secretsmanager:us-east-1:123456789012:secret:sentinel-ccc";
        const JSON_PASSWORD: &str = "json-sentinel-ddd";
        let db = crate::db::test_db().await;
        let secrets = MockSecrets::holding("");
        let signer = Arc::new(ClockSigner {
            token_override: std::sync::Mutex::new(Some(TOKEN.to_string())),
            ..ClockSigner::plain(&Arc::new(AtomicU64::new(0)))
        });
        let stores = Stores::with_providers(
            db.pool.clone(),
            settings(),
            Providers::new(
                Arc::new(|_| Some(PASSWORD.to_string())),
                Arc::clone(&secrets) as Arc<dyn SecretReader>,
                Arc::clone(&signer) as Arc<dyn TokenSigner>,
            ),
        );
        let name = StoreName::parse("shared-1").unwrap();
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        let _guard = crate::test_tracing::set_default(subscriber);

        // Each case: the row's kind and reference, and what the AWS side
        // answers. Every one ends in a failed open.
        let cases: [(&str, &str, Result<String, CredentialError>); 5] = [
            ("env", "SENTINEL_VARIABLE", Ok(String::new())),
            (
                "secrets-manager",
                SECRET_ARN,
                Err(CredentialError::Service {
                    service: "secrets manager",
                    code: "AccessDeniedException".to_string(),
                }),
            ),
            (
                "secrets-manager",
                SECRET_ARN,
                Ok(format!(
                    r#"{{"username":"nobody","password":"{JSON_PASSWORD}"}}"#
                )),
            ),
            (
                "secrets-manager",
                SECRET_ARN,
                Ok(format!("not json {JSON_PASSWORD}")),
            ),
            ("iam", "nobody", Ok(String::new())),
        ];
        for round in 0..2 {
            if round == 1 {
                // Everything above warmed the callsites; recompute their
                // cached interest against this subscriber, then capture.
                tracing::callsite::rebuild_interest_cache();
                captured.clear();
            }
            let mut replies = String::new();
            for (kind, reference, answer) in &cases {
                secrets.set(match answer {
                    Ok(text) => Ok(text.clone()),
                    Err(CredentialError::Service { service, code }) => {
                        Err(CredentialError::Service {
                            service,
                            code: code.clone(),
                        })
                    }
                    Err(_) => unreachable!("only service errors are used"),
                });
                add_row(&db.pool, "shared-1", kind, reference).await;
                let failed = stores.get(&name).await.unwrap_err();
                assert!(
                    matches!(failed, OkfError::StoreUnavailable { .. }),
                    "{kind}: {failed:?}"
                );
                replies.push_str(&failed.to_json().to_string());
                replies.push_str(&format!("{failed} {failed:?}"));
                sqlx::query("DELETE FROM stores WHERE name = 'shared-1'")
                    .execute(&db.pool)
                    .await
                    .unwrap();
            }
            let logs = captured.text();
            for private in [
                PASSWORD,
                TOKEN,
                JSON_PASSWORD,
                SECRET_ARN,
                "sentinel-ccc",
                "123456789012",
            ] {
                assert!(
                    !logs.contains(private),
                    "{private} reached the log:\n{logs}"
                );
                assert!(!replies.contains(private), "{private} reached a reply");
            }
            if round == 1 {
                // The capture works: the failures themselves were logged.
                assert!(logs.contains("could not"), "{logs}");
            }
        }
    }
}

//! The server's shared state (design "Architecture"): the database, the
//! configuration, the clock, the key cache, the catalog directory and the
//! catalog cache (catalogs load on demand, M18 Decisions 2 and 6), the
//! change feed's handlers, the poll loop that still reloads the key cache
//! (design "Noticing changes made outside the server"), and the clock check
//! loop and its last result (design "Time").

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};

use sqlx::PgPool;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::auth::{Admission, Keys};
use crate::cache::catalogs::{CatalogCache, CatchUp};
use crate::cache::directory::CatalogDirectory;
use crate::catalog::CatalogState;
use crate::clock::Clock;
use crate::clock_check::{self, ClockStatus};
use crate::config::Config;
use crate::error::OkfError;
use crate::http::limits::SignInLimits;
use crate::http::passkeys::Passkeys;
use crate::http::sessions::SessionBindings;
use crate::keyed_lock::{KeyedGuard, KeyedLocks};
use crate::oauth::OAuth;
use crate::oidc::OidcProviders;
use crate::overlay::Overlays;
use crate::shared;
use crate::store::audit::{self, AccessKind, AccessRecord};
use crate::store::catalogs::{CatalogGeneration, CatalogRow};
use crate::store::settings::{self, Settings};
use crate::store::{catalogs, unlocks};
use crate::telemetry::metrics::{AnnouncementResult, Metrics, Outcome};
use crate::telemetry::subscriber::split_event;

/// The state every route and tool shares. Built once by [`App::load`] and
/// shared as an `Arc<App>`.
pub struct App {
    pool: PgPool,
    config: Config,
    // `Arc<dyn Clock>`: any clock (the real one, or a fixed one in tests),
    // shared between threads. `dyn Clock` is a trait object: Python's duck
    // typing, with the method looked up at run time.
    clock: Arc<dyn Clock>,
    keys: Arc<Keys>,
    // Which key opened each MCP session (M5 Decision 2).
    sessions: SessionBindings,
    // The shared Redis (`[redis]`); `None` keeps every short-lived state in
    // this process's memory (M16).
    redis: Option<shared::Redis>,
    // Each open changeset's overlay, cached (M5 Decision 6).
    overlays: Overlays,
    // Each changeset's writers, queued in this process before they take a
    // pooled connection (`write::apply`).
    write_queue: KeyedLocks,
    // The in-process half of each catalog's publish lock (design
    // "Publish"), by catalog id: here rather than in `CatalogState`, so
    // every state of one catalog shares it (M18 Decision 4).
    publish_locks: KeyedLocks,
    // Each catalog's rebuild lock (`CatalogState::refresh`), by catalog id,
    // for the same reason.
    rebuild_locks: KeyedLocks,
    // Catalog rows by name with their access facts (M18 Decision 6): what
    // "does this catalog exist" and every snapshot's freshness are read from.
    directory: CatalogDirectory,
    // Catalog snapshots by id, loaded on demand and bounded in estimated
    // bytes (M18 Decision 2).
    catalog_cache: CatalogCache,
    // The `admin_generation` the key cache was loaded at. Written only under
    // `polling`, so `Relaxed` ordering is enough.
    admin_generation: AtomicI64,
    // The last `admin_generation` this instance published over Redis (or the
    // one it started at). Tracked apart from `admin_generation` so a reload
    // that only loaded a change (and did not announce) cannot make the next
    // announcing reload skip it. Written only under `polling`.
    announced_generation: AtomicI64,
    // The effective sign-in policy (`[auth]` with `server_settings`
    // overrides), reloaded with the key cache whenever `admin_generation`
    // moves. The lock is held only to clone or swap the `Arc`.
    settings: RwLock<Arc<Settings>>,
    // Held for a whole `poll` pass. Without it, two overlapping passes (the
    // loop and a test's or a later milestone's direct call) could swap an
    // older key list in after a newer one while storing the newer counter,
    // and a revoked key would keep working until the next admin change.
    // tokio's `Mutex`: it is held across `.await`s.
    polling: tokio::sync::Mutex<()>,
    // The last clock check (design "Time"); `None` until the first one, and
    // always without a `time_server`. Written by the check loop only.
    clock_status: RwLock<Option<ClockStatus>>,
    // The OAuth authorization server's state; `None` without `public_url`.
    oauth: Option<OAuth>,
    // The sign-in routes' per-address and per-email limits.
    sign_in_limits: SignInLimits,
    // The configured mail provider (Decision 10), built once at start-up.
    mailer: Arc<dyn crate::mail::Mailer>,
    // The Forminit client `send_feedback`'s worker posts with; `None`
    // without `[feedback]` (issue #674).
    forminit: Option<Arc<crate::mail::feedback::Forminit>>,
    // The WebAuthn relying party and ceremonies; `None` without a domain
    // `public_url`.
    passkeys: Option<Passkeys>,
    // The configured OIDC providers; `None` without `public_url` or providers.
    oidc: Option<OidcProviders>,
    // Background writes of recorded accesses (admin reads). A `JoinSet`
    // (a set of spawned tasks that can be awaited together), so tests and
    // shutdown can wait for them; a `std` mutex, held only to spawn into it
    // or to take it out.
    access_writes: std::sync::Mutex<tokio::task::JoinSet<()>>,
    // The metric instruments (M15 Task 2); built on the global meter provider
    // unless a test supplies its own.
    metrics: Metrics,
    // Test hook: a background access write waits for this lock first, so a
    // test can hold it to run a deletion or a transfer between admission
    // and persistence.
    #[cfg(test)]
    pub(crate) access_hold: Arc<tokio::sync::Mutex<()>>,
}

impl App {
    /// Loads the key cache and the effective settings. No catalog is
    /// loaded: each loads into the catalog cache on its first use (M18 spec
    /// Decision 2), so start-up does not grow with the number of catalogs.
    ///
    /// # Arguments
    /// * `pool` - the database (migrated).
    /// * `config` - the server's settings.
    /// * `clock` - the source of every timestamp.
    ///
    /// # Returns
    /// The shared state.
    ///
    /// # Errors
    /// [`OkfError::Config`] (path `[redis]`) from [`shared::Redis::connect`]
    /// when `[redis]` is set and its variable is unset or blank, its value is
    /// not a Redis URL, or Redis does not answer `PING`;
    /// [`OkfError::Database`] reading the counter, the keys or the settings;
    /// [`OkfError::Config`] from [`OAuth::new`] or [`Passkeys::new`], or from
    /// [`crate::mail::from_config`] (a mail secret missing from the
    /// environment, or no AWS region for SES) or [`OidcProviders::new`] (an
    /// OIDC client secret missing from the environment).
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn load(
        pool: PgPool,
        config: Config,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>, OkfError> {
        Self::load_with_metrics(pool, config, clock, Metrics::global()).await
    }

    /// [`App::load`] with the metric instruments given: tests pass ones built
    /// on a local provider, so they can read what a real `App` recorded.
    ///
    /// # Arguments
    /// * `pool` - the database (migrated).
    /// * `config` - the server's settings.
    /// * `clock` - the source of every timestamp.
    /// * `metrics` - the instruments the app records on.
    ///
    /// # Returns
    /// The shared state.
    ///
    /// # Errors
    /// As [`App::load`].
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn load_with_metrics(
        pool: PgPool,
        config: Config,
        clock: Arc<dyn Clock>,
        metrics: Metrics,
    ) -> Result<Arc<Self>, OkfError> {
        // `[redis]` set: connect now, so a bad URL or an unreachable Redis
        // refuses start-up. The URL comes from the named variable only.
        let redis = match &config.redis {
            Some(table) => Some(
                shared::Redis::connect(table, &|name| std::env::var(name).ok(), &metrics).await?,
            ),
            None => None,
        };
        // Boxed so this future holds a pointer, not the whole of the load's
        // future: callers nest `App::load` inside big test futures, and the
        // extra level pushed rustc's layout query past its recursion limit.
        Box::pin(Self::load_with_redis(pool, config, clock, metrics, redis)).await
    }

    /// [`App::load_with_metrics`] with the Redis handle given (or none):
    /// tests pass one handle to two apps, as two replicas share one Redis.
    ///
    /// # Arguments
    /// * `pool`, `config`, `clock`, `metrics` - as for `load_with_metrics`.
    /// * `redis` - the shared Redis; `None` keeps everything in memory.
    ///
    /// # Returns
    /// The shared state.
    ///
    /// # Errors
    /// As [`App::load`], except that no `[redis]` connection is made here.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn load_with_redis(
        pool: PgPool,
        config: Config,
        clock: Arc<dyn Clock>,
        metrics: Metrics,
        redis: Option<shared::Redis>,
    ) -> Result<Arc<Self>, OkfError> {
        // Read the counter before the keys: a change committed between the
        // two moves it again, so the next poll reloads.
        let admin_generation = catalogs::admin_generation(&pool).await?;
        // `clock` moves into `Self` below, so the key cache gets its own handle.
        let keys = Arc::new(Keys::load(&pool, Arc::clone(&clock)).await?);
        let settings = settings::load(&pool, &config.auth).await?;
        let oauth = OAuth::new(&config, redis.as_ref())?;
        let sessions = SessionBindings::new(redis.as_ref(), config.session_idle_timeout);
        let overlays = Overlays::new(config.session_idle_timeout, config.overlay_cache_size);
        let mailer = crate::mail::from_config(&config.mail).await?;
        // A `[feedback] api_key_env` that names an unset variable fails
        // start-up here, as a mail secret does.
        let forminit = config
            .feedback
            .as_ref()
            .map(|feedback| {
                crate::mail::feedback::Forminit::from_config(feedback, &|name| {
                    std::env::var(name).ok()
                })
                .map(Arc::new)
            })
            .transpose()?;
        let passkeys = Passkeys::new(&config, redis.as_ref())?;
        // A provider whose secret variable is unset fails start-up here.
        let oidc = OidcProviders::new(&config, |name| std::env::var(name).ok(), redis.as_ref())?;
        let directory = CatalogDirectory::new(
            pool.clone(),
            config.catalog_directory_entries,
            config.catalog_ttl,
            metrics.clone(),
        );
        // `if let Some(name) = ...`: Python's `if (name := ...) is not None`.
        // One directory read, not a load: the catalog loads when first used.
        if let Some(name) = &config.default_catalog {
            if directory.resolve(name).await?.is_none() {
                tracing::warn!(catalog = %name, "default_catalog does not exist (yet)");
            }
        }
        let catalog_cache = CatalogCache::new(
            pool.clone(),
            config.catalog_cache_bytes.as_u64(),
            metrics.clone(),
        );
        let app = Arc::new(Self {
            pool,
            config,
            clock,
            keys,
            sessions,
            redis,
            overlays,
            write_queue: KeyedLocks::new(),
            publish_locks: KeyedLocks::new(),
            rebuild_locks: KeyedLocks::new(),
            directory,
            catalog_cache,
            admin_generation: AtomicI64::new(admin_generation),
            announced_generation: AtomicI64::new(admin_generation),
            settings: RwLock::new(Arc::new(settings)),
            polling: tokio::sync::Mutex::new(()),
            clock_status: RwLock::new(None),
            oauth,
            sign_in_limits: SignInLimits::new(),
            mailer,
            forminit,
            passkeys,
            oidc,
            access_writes: std::sync::Mutex::new(tokio::task::JoinSet::new()),
            metrics,
            #[cfg(test)]
            access_hold: Arc::default(),
        });
        // `Arc::downgrade`: the gauges' callbacks live in the meter provider,
        // which outlives the app, so they hold a weak handle (no cycle).
        app.metrics.observe_pool(Arc::downgrade(&app));
        Ok(app)
    }

    /// Records one operator access in the background, off the request path
    /// (plan Decision 5): an admin read's hourly bucket, or an unlock's
    /// read, write or export tally. A failure is
    /// logged; [`App::settle_access`] waits for every write.
    ///
    /// # Arguments
    /// * `record` - the access, with identities and scope fixed at admission.
    /// * `kind` - read, write or export.
    /// * `admission` - how it was let in.
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn note_access(&self, record: AccessRecord, kind: AccessKind, admission: Admission) {
        let pool = self.pool.clone();
        #[cfg(test)]
        let hold = Arc::clone(&self.access_hold);
        let mut writes = self
            .access_writes
            .lock()
            .expect("access writes lock poisoned");
        // Reap the finished ones, so the set holds only running writes.
        while writes.try_join_next().is_some() {}
        // `async move` hands `pool` and `record` to the task, which outlives
        // this call.
        writes.spawn(async move {
            #[cfg(test)]
            let _held = hold.lock().await;
            let written = match admission {
                Admission::AdminRead => audit::record_admin_read(&pool, &record).await,
                // The unlock id was fixed at admission; the record is not
                // needed for a tally.
                Admission::Unlocked { unlock_id } => unlocks::tally(&pool, unlock_id, kind).await,
                Admission::Customer | Admission::Untracked | Admission::Maintenance => Ok(()),
            };
            if let Err(error) = written {
                split_event!(
                    error,
                    { catalog = %record.catalog, ?kind, },
                    { %error, },
                    "recording an operator access failed"
                );
            }
        });
    }

    /// Waits for every background access write started so far (tests, and
    /// shutdown, so a stopping server loses no count).
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn settle_access(&self) {
        // Take the set out under the lock, then wait without holding it
        // (`mem::take` leaves an empty set in its place).
        let mut writes = std::mem::take(
            &mut *self
                .access_writes
                .lock()
                .expect("access writes lock poisoned"),
        );
        while writes.join_next().await.is_some() {}
    }

    /// The shared Redis; `None` without `[redis]`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line accessor")
    )]
    pub fn redis(&self) -> Option<&shared::Redis> {
        self.redis.as_ref()
    }

    /// The catalog directory: rows by name with their access facts.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line accessor")
    )]
    pub fn directory(&self) -> &CatalogDirectory {
        &self.directory
    }

    /// The catalog cache: snapshots by catalog id, loaded on demand.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line accessor")
    )]
    pub fn catalog_cache(&self) -> &CatalogCache {
        &self.catalog_cache
    }

    /// The instruments this app records on.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line accessor")
    )]
    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// The database.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The server's settings.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The source of every timestamp.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// The sign-in routes' rate limits.
    ///
    /// # Returns
    /// The limiters.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn sign_in_limits(&self) -> &SignInLimits {
        &self.sign_in_limits
    }

    /// The clock as a shared handle, for tasks that outlive a borrow of
    /// `App` (the mail worker).
    ///
    /// # Returns
    /// The same clock [`App::clock`] lends.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn clock_handle(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }

    /// The configured mail provider.
    ///
    /// # Returns
    /// The mailer the mail worker sends with.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn mailer(&self) -> &Arc<dyn crate::mail::Mailer> {
        &self.mailer
    }

    /// The Forminit client the feedback worker posts with.
    ///
    /// # Returns
    /// The client, or `None` without `[feedback]`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn forminit(&self) -> Option<&Arc<crate::mail::feedback::Forminit>> {
        self.forminit.as_ref()
    }

    /// The OIDC providers.
    ///
    /// # Returns
    /// `None` when none are configured (or there is no `public_url`).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn oidc(&self) -> Option<&OidcProviders> {
        self.oidc.as_ref()
    }

    /// The key cache (shared with the auth layer).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn keys(&self) -> &Arc<Keys> {
        &self.keys
    }

    /// The effective sign-in policy, as of the last key reload. Request
    /// handlers read it here, never from `server_settings`.
    ///
    /// # Returns
    /// A shared snapshot (cheap: an `Arc` clone, one atomic increment).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn settings(&self) -> Arc<Settings> {
        Arc::clone(&self.settings.read().expect("settings lock poisoned"))
    }

    /// Which key opened each MCP session (the `/mcp` session binding).
    ///
    /// # Returns
    /// The bindings, shared by every `/mcp` request.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn sessions(&self) -> &SessionBindings {
        &self.sessions
    }

    /// The OAuth state.
    ///
    /// # Returns
    /// `None` when OAuth is off (no `public_url`).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn oauth(&self) -> Option<&OAuth> {
        self.oauth.as_ref()
    }

    /// The passkey state.
    ///
    /// # Returns
    /// `None` when passkeys are off (no `public_url`, or an address host).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn passkeys(&self) -> Option<&Passkeys> {
        self.passkeys.as_ref()
    }

    /// The overlay cache (M5 Decision 6).
    ///
    /// # Returns
    /// Every open changeset's cached overlay, shared by every tool call.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn overlays(&self) -> &Overlays {
        &self.overlays
    }

    /// The per-changeset write queue: `write::apply` holds a changeset's
    /// lock here before it begins its transaction, so writers waiting their
    /// turn on one changeset hold no pooled connection.
    ///
    /// # Returns
    /// The locks, keyed by changeset id.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn write_queue(&self) -> &KeyedLocks {
        &self.write_queue
    }

    /// Waits for the in-process half of a catalog's publish lock (design
    /// "Publish"): one publish or discard of the catalog at a time in this
    /// server. Taken before any pooled connection or Postgres lock; the
    /// other half is the advisory lock in the publish transaction.
    ///
    /// # Arguments
    /// * `catalog_id` - the catalog's database id ([`CatalogState::id`]).
    ///
    /// # Returns
    /// The guard; the lock is held until it is dropped.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn publish_lock(&self, catalog_id: i64) -> KeyedGuard<'_> {
        self.publish_locks.lock(catalog_id).await
    }

    /// The catalog rebuild locks, by catalog id, that
    /// [`CatalogState::refresh`] takes so one catalog builds one snapshot at
    /// a time, whichever of its states asks.
    ///
    /// # Returns
    /// The locks.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn rebuild_locks(&self) -> &KeyedLocks {
        &self.rebuild_locks
    }

    /// The last clock check, if any (`/healthz` shows it).
    ///
    /// # Returns
    /// A copy of the status, or `None` without a `time_server` or before the
    /// first check.
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn clock_status(&self) -> Option<ClockStatus> {
        // A std lock is "poisoned" when a thread panicked while holding it;
        // nothing here can panic under the lock, so that is an invariant.
        self.clock_status
            .read()
            .expect("clock status lock poisoned")
            .clone()
    }

    /// Records a clock check.
    ///
    /// # Arguments
    /// * `status` - the check's outcome.
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn set_clock_status(&self, status: ClockStatus) {
        *self
            .clock_status
            .write()
            .expect("clock status lock poisoned") = Some(status);
    }

    /// The directory row of catalog `name`: whether it exists, and its
    /// owner, description and access facts, without loading its content.
    ///
    /// # Arguments
    /// * `name` - the catalog's name.
    ///
    /// # Returns
    /// The row, or `None` if there is no such catalog.
    ///
    /// # Errors
    /// As [`CatalogDirectory::resolve`].
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn catalog_row(&self, name: &str) -> Result<Option<Arc<CatalogRow>>, OkfError> {
        self.directory.resolve(name).await
    }

    /// The state of the catalog `row` describes, at `row.generation` or
    /// newer: from the catalog cache, refreshed in place, or loaded. The
    /// state is that incarnation's (`row.id`), never another catalog's
    /// created since under the name.
    ///
    /// # Arguments
    /// * `row` - the catalog's directory row ([`App::catalog_row`]).
    ///
    /// # Returns
    /// The state.
    ///
    /// # Errors
    /// As [`CatalogCache::get`]: [`OkfError::CatalogNotSelected`] for a
    /// catalog deleted since, or whose stored data does not build;
    /// [`OkfError::Database`].
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn catalog_state(&self, row: &CatalogRow) -> Result<Arc<CatalogState>, OkfError> {
        self.catalog_cache.get(row, &self.rebuild_locks).await
    }

    /// [`App::catalog_state`] for a listing's row: the state for a listed
    /// catalog's snapshot fields, or `None` for one to leave out of the
    /// page because it was deleted since the page was read or its stored
    /// data does not build (as a catalog that was not loaded was left out
    /// before milestone 18).
    ///
    /// # Arguments
    /// * `row` - a listed catalog's row.
    ///
    /// # Returns
    /// The state, or `None`.
    ///
    /// # Errors
    /// [`OkfError::Database`], or [`OkfError::Busy`] for a load shared with
    /// another request that failed.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn listed_state(
        &self,
        row: &CatalogRow,
    ) -> Result<Option<Arc<CatalogState>>, OkfError> {
        match self.catalog_state(row).await {
            Ok(state) => Ok(Some(state)),
            Err(OkfError::CatalogNotSelected { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The state of catalog `name`: [`App::catalog_row`], then
    /// [`App::catalog_state`]. For a caller that has only a name and no
    /// request context (the archive routes, tests).
    ///
    /// # Arguments
    /// * `name` - the catalog's name.
    ///
    /// # Returns
    /// The state, or `None` if there is no such catalog, or it was deleted
    /// meanwhile, or its stored data does not build (as a catalog that was
    /// not loaded answered before).
    ///
    /// # Errors
    /// [`OkfError::Database`], or [`OkfError::Busy`] for a load shared with
    /// another request that failed.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn catalog(&self, name: &str) -> Result<Option<Arc<CatalogState>>, OkfError> {
        match self.catalog_row(name).await? {
            Some(row) => self.listed_state(&row).await,
            None => Ok(None),
        }
    }

    /// One pass of the poll loop: reloads the key cache and the settings if
    /// `admin_generation` moved. Catalogs are not polled: the change feed
    /// refreshes a cached catalog, and every use checks its snapshot against
    /// the directory row (M18 Task 8). Pruning and the cluster counts are
    /// housekeeping jobs ([`crate::housekeeping`]), run once per cluster, not
    /// per pass.
    ///
    /// A key reload (or the `admin_generation` read before it) that fails is
    /// logged and retried at the next pass. Passes never overlap: a call
    /// made while another runs waits for it, then makes its own pass.
    ///
    /// # Returns
    /// `Ok(())` once the pass is done.
    ///
    /// # Errors
    /// None today; the `Result` stays for the callers (`settle`).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "makes the okf.poll root span itself")
    )]
    pub async fn poll(&self) -> Result<(), OkfError> {
        let started = std::time::Instant::now();
        // `parent: None` makes each pass the root of its own trace, whatever
        // span the caller is in (spec "Background roots").
        let result = self
            .poll_pass()
            .instrument(tracing::info_span!(parent: None, "okf.poll"))
            .await;
        let outcome = if result.is_ok() {
            Outcome::Ok
        } else {
            Outcome::Error
        };
        self.metrics.poll(outcome, started.elapsed());
        result
    }

    /// The body of [`App::poll`], inside its `okf.poll` span.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "runs inside the okf.poll span")
    )]
    async fn poll_pass(&self) -> Result<(), OkfError> {
        let _pass = self.polling.lock().await;
        self.reload_keys_if_changed(true).await;
        Ok(())
    }

    /// Reloads the key cache and the settings if `admin_generation` moved
    /// since they were loaded, serialized with poll passes through `polling`.
    /// This is the announcement path: a subscriber calls it on a message or
    /// a resubscribe, and a caller about to refuse a credential calls it
    /// through [`App::catch_up_keys`].
    ///
    /// # Arguments
    /// * `announce` - publish the loaded generation if it was not announced
    ///   yet: `false` when this reload answers an announcement (the
    ///   announcer already told everyone).
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn reload_keys_now(&self, announce: bool) {
        let _pass = self.polling.lock().await;
        self.reload_keys_if_changed(announce).await;
    }

    /// Before refusing a credential whose row is live but whose grant is not
    /// in the key cache (spec Decision 14): reloads if `admin_generation`
    /// moved. A no-op without Redis, where one replica polls after its own
    /// changes.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn catch_up_keys(&self) {
        if self.redis.is_some() {
            self.reload_keys_now(false).await;
        }
    }

    /// The `admin_generation` the key cache was loaded at.
    ///
    /// # Returns
    /// The generation.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "one-line accessor: a span would record nothing"
        )
    )]
    pub fn loaded_admin_generation(&self) -> i64 {
        self.admin_generation.load(Ordering::Relaxed)
    }

    /// Tells the other replicas that a catalog's published generation moved
    /// (issue #659), after the write that moved it committed. Best effort and
    /// detached: the publish happens on its own task, so a slow or failing
    /// Redis never delays the caller's answer; a failure is counted and
    /// logged, and the other replicas' poll loops cover it. Because nothing
    /// waits for the publish or the other replicas' refresh, this is prompt
    /// eventual consistency, not read-your-writes: the caller's next read on
    /// another replica may still see the old snapshot. Does nothing without
    /// `[redis]`.
    ///
    /// # Arguments
    /// * `change` - the catalog and the generation its write committed.
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn announce_catalog(&self, change: CatalogGeneration) {
        let Some(redis) = self.redis.clone() else {
            return;
        };
        // `in_current_span` keeps the publish under the caller's trace,
        // although the task may outlive the caller.
        tokio::spawn(shared::announce::publish_catalog(redis, change).in_current_span());
    }

    /// Answers one catalog announcement: refreshes the cached catalog with
    /// that id unless its snapshot already reached the announced
    /// generation ([`CatalogCache::catch_up`]). Each call is the root of its
    /// own trace (`okf.catalog.announcement`) and is counted in
    /// `okf.catalog.announcements`.
    ///
    /// A catalog this replica has not cached is left alone (it loads when
    /// first used), and so is one deleted since: announcements never load
    /// anything (no load storm). Concurrent refreshes of one catalog, from
    /// here, the change feed, a request or a local publish, are serialized
    /// by [`CatalogState::refresh`]'s rebuild lock: the first rebuilds, the
    /// rest see the generation already reached and return, so a snapshot
    /// never moves back and a generation is built once.
    ///
    /// # Arguments
    /// * `change` - the announced catalog and generation.
    ///
    /// # Returns
    /// What the announcement came to.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "makes the okf.catalog.announcement root span itself"
        )
    )]
    pub async fn apply_catalog_announcement(
        &self,
        change: CatalogGeneration,
    ) -> AnnouncementResult {
        // `parent: None`: a background root, like each poll pass.
        let span = tracing::info_span!(
            parent: None,
            "okf.catalog.announcement",
            okf.catalog = tracing::field::Empty,
            okf.announcement.result = tracing::field::Empty,
        );
        let result = self
            .refresh_announced(change, &span)
            .instrument(span.clone())
            .await;
        span.record("okf.announcement.result", result.as_str());
        self.metrics.catalog_announcement(result);
        result
    }

    /// The body of [`App::apply_catalog_announcement`], inside its span.
    ///
    /// # Arguments
    /// * `change` - the announced catalog and generation.
    /// * `span` - the announcement's span, to record the catalog's name on.
    ///
    /// # Returns
    /// What the announcement came to.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn refresh_announced(
        &self,
        change: CatalogGeneration,
        span: &tracing::Span,
    ) -> AnnouncementResult {
        if let Some(state) = self.catalog_cache.resident(change.catalog_id).await {
            span.record("okf.catalog", state.name());
        }
        let caught_up = self
            .catalog_cache
            .catch_up(change.catalog_id, change.generation, &self.rebuild_locks)
            .await;
        match caught_up {
            Ok(CatchUp::Applied) => AnnouncementResult::Applied,
            Ok(CatchUp::Current) => AnnouncementResult::Current,
            Ok(CatchUp::NotResident) | Err(OkfError::CatalogNotSelected { .. }) => {
                AnnouncementResult::Unknown
            }
            Err(error) => {
                split_event!(
                    error,
                    { catalog_id = change.catalog_id, kind = error.kind(), },
                    { %error, },
                    "refresh after an announcement failed; the next use retries"
                );
                AnnouncementResult::Failed
            }
        }
    }

    /// Applies one change-feed message to this replica's caches: the
    /// directory, the catalog cache, and (until milestone 18's credential
    /// caches replace it) the key cache.
    ///
    /// A `Catalog` message refreshes that catalog if it is cached and behind
    /// (the body of [`App::apply_catalog_announcement`]). A `CatalogMeta` message
    /// for a catalog that no longer exists under that id (deleted) drops it
    /// from the catalog cache; a catalog that merely changed its
    /// description, owner or maintenance keeps its snapshot, since those
    /// live in the directory row. Every kind but `Catalog` reloads the key
    /// cache and settings.
    ///
    /// # Arguments
    /// * `message` - what changed.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn apply(&self, message: &crate::invalidate::Message) {
        use crate::invalidate::Invalidation;
        // The directory first: it is a few map operations, and the catalog
        // cache below reads the row it now holds.
        self.directory.apply(&message.invalidation).await;
        match &message.invalidation {
            Invalidation::Catalog { id, generation } => {
                // The announcement's body, without its span and counter:
                // this is the change feed, counted by its own metrics.
                self.refresh_announced(
                    CatalogGeneration {
                        catalog_id: *id,
                        generation: *generation,
                    },
                    &tracing::Span::none(),
                )
                .await;
            }
            Invalidation::CatalogMeta { id, name } => {
                self.forget_if_deleted(*id, name).await;
                self.reload_keys_now(false).await;
            }
            Invalidation::Credential { .. }
            | Invalidation::User { .. }
            | Invalidation::Team { .. }
            | Invalidation::Settings => self.reload_keys_now(false).await,
        }
    }

    /// Drops catalog `id` from the catalog cache if `name` no longer names
    /// it (the catalog was deleted, perhaps created again under the name).
    /// A catalog that is not cached costs nothing; one that is costs one
    /// directory read, at the rate of administrative writes.
    ///
    /// # Arguments
    /// * `id` - the catalog's id, from the message.
    /// * `name` - its name, from the message.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn forget_if_deleted(&self, id: i64, name: &str) {
        if self.catalog_cache.resident(id).await.is_none() {
            return;
        }
        // A failed read is treated as gone: the entry is only a cache, and
        // the next use loads it again if the catalog is still there.
        let still_there = matches!(
            self.directory.resolve(name).await,
            Ok(Some(row)) if row.id == id
        );
        if !still_there {
            self.catalog_cache.forget(id).await;
        }
    }

    /// After the change feed's connection was lost (or a payload could not
    /// be read): messages sent meanwhile are gone, so every cache is
    /// suspect (spec Decision 12). The directory's rows are marked stale
    /// (each is read again on its next use), and with them every cached
    /// snapshot, which each use checks against its row; the key cache and
    /// settings are reloaded.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn apply_reconnect(&self) {
        self.catch_up_catalogs().await;
        self.reload_keys_now(false).await;
    }

    /// Makes every cached catalog revalidate on its next use: what the
    /// catalog subscriber does when it (re)subscribes, since announcements
    /// sent while it was not subscribed are lost, and what a lost change
    /// feed does. The directory's rows are marked stale, so each is read
    /// again on its next use, and [`App::catalog_state`] refreshes a
    /// snapshot that is behind its row. Nothing is read now (no load storm).
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn catch_up_catalogs(&self) {
        self.directory.mark_all_stale().await;
    }

    /// Reloads the key cache and the settings if `admin_generation` moved
    /// since they were loaded, then, when `announce` is set, publishes the
    /// loaded generation if it differs from the last one announced.
    /// Called only under `polling`.
    ///
    /// Failing to read the counter or to reload is logged, not returned: the
    /// stored counter stays as it was, so the next pass tries again, and the
    /// caller's catalog changes go ahead regardless.
    ///
    /// Announcing is tracked apart from loading. A reload that loads a change
    /// without announcing it (the subscriber's resubscribe, `catch_up_keys`)
    /// must not make the `settle` poll that follows find nothing to publish,
    /// so the announce step runs whether or not this call reloaded.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn reload_keys_if_changed(&self, announce: bool) {
        self.reload_moved_keys().await;
        if !announce {
            return;
        }
        let Some(redis) = &self.redis else {
            return;
        };
        let loaded = self.admin_generation.load(Ordering::Relaxed);
        // Under `polling`, so no other reload moves either counter meanwhile.
        if loaded != self.announced_generation.load(Ordering::Relaxed) {
            shared::announce::publish(redis, Some(loaded)).await;
            // Stored even if the publish failed: the failure is counted, and
            // the poll loop covers a lost announcement (spec Decision 12).
            self.announced_generation.store(loaded, Ordering::Relaxed);
        }
    }

    /// The reload half of [`App::reload_keys_if_changed`]. Called only under
    /// `polling`.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn reload_moved_keys(&self) {
        let admin_generation = match catalogs::admin_generation(&self.pool).await {
            Ok(admin_generation) => admin_generation,
            Err(error) => {
                split_event!(
                    error,
                    { kind = error.kind(), },
                    { %error, },
                    "reading admin_generation failed; retrying at the next poll"
                );
                return;
            }
        };
        if admin_generation == self.admin_generation.load(Ordering::Relaxed) {
            return;
        }
        // An async block lets `?` stop at the first failure; either way the
        // counter is stored only when both were reloaded, so a failure is
        // retried at the next pass. A stored setting this server cannot
        // read is not a failure: `settings::load` falls back to the default.
        let reloaded = async {
            self.keys.reload(&self.pool).await?;
            let settings = settings::load(&self.pool, &self.config.auth).await?;
            *self.settings.write().expect("settings lock poisoned") = Arc::new(settings);
            Ok::<(), OkfError>(())
        }
        .instrument(tracing::info_span!("okf.keycache.reload"))
        .await;
        self.metrics.keycache_reload(if reloaded.is_ok() {
            Outcome::Ok
        } else {
            Outcome::Error
        });
        match reloaded {
            Ok(()) => self
                .admin_generation
                .store(admin_generation, Ordering::Relaxed),
            Err(error) => {
                split_event!(
                    error,
                    { kind = error.kind(), },
                    { %error, },
                    "key or settings reload failed; retrying at the next poll"
                );
            }
        }
    }

    /// Runs [`App::poll`] every `poll_interval` until `shutdown` is cancelled.
    /// A failed pass is logged and the loop goes on.
    ///
    /// # Arguments
    /// * `shutdown` - cancelled when the server stops.
    // `self: Arc<Self>`: the method takes a shared handle by value, so the
    // spawned task owns one and the `App` lives as long as the loop.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "long-lived loop: its span would never close and would stay open as the parent of every later span"
        )
    )]
    pub async fn poll_until(self: Arc<Self>, shutdown: CancellationToken) {
        let mut ticks = tokio::time::interval(self.config.poll_interval);
        // After a slow pass, wait a full interval instead of catching up.
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // The first tick completes at once, and the state was just loaded.
        ticks.tick().await;
        loop {
            // `select!` waits for whichever branch is ready first and drops
            // the other. A pass in progress is not interrupted: cancellation
            // is only noticed between passes.
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = ticks.tick() => {
                    if let Err(error) = self.poll().await {
                        split_event!(
                            error,
                            { kind = error.kind(), },
                            { %error, },
                            "poll failed"
                        );
                    }
                }
            }
        }
    }

    /// Checks the clock at once and then every [`clock_check::CHECK_INTERVAL`]
    /// until `shutdown` is cancelled. Returns at once without a `time_server`.
    ///
    /// # Arguments
    /// * `shutdown` - cancelled when the server stops.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "long-lived loop: its span would never close and would stay open as the parent of every later span"
        )
    )]
    pub async fn check_clock_until(self: Arc<Self>, shutdown: CancellationToken) {
        // `let ... else`: return when there is no server to ask.
        let Some(server) = self.config.time_server.clone() else {
            return;
        };
        let mut ticks = tokio::time::interval(clock_check::CHECK_INTERVAL);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            // The first tick completes at once: the startup check.
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = ticks.tick() => {}
            }
            // An async call does nothing until awaited: `check` is the
            // query, not yet started, for the `select!` below to drive.
            // Each check is the root of its own trace, whatever started the
            // loop (spec "Background roots").
            // Kept as a handle: the result is recorded on this span, not on
            // whatever span is current when the check finishes.
            let root = tracing::info_span!(
                parent: None,
                "okf.clock.check",
                okf.outcome = tracing::field::Empty,
                okf.clock.offset = tracing::field::Empty,
                otel.status_code = tracing::field::Empty,
            );
            let check = clock_check::check_once(&server, self.clock(), clock_check::QUERY_TIMEOUT)
                .instrument(root.clone());
            // A stop during a query ends it at once instead of waiting up to
            // `QUERY_TIMEOUT` (Brooke's review of PR #138).
            tokio::select! {
                () = shutdown.cancelled() => return,
                status = check => {
                    let outcome = if status.ok { Outcome::Ok } else { Outcome::Error };
                    self.metrics.clock_check(outcome, status.offset_ms);
                    root.record("okf.outcome", outcome.as_str());
                    match status.offset_ms {
                        // Seconds, as an `f64`, like the gauge.
                        Some(offset_ms) => {
                            root.record("okf.clock.offset", offset_ms as f64 / 1000.0);
                        }
                        // A failed check is a failed trace: the WARN event
                        // alone does not set the span's status.
                        None => {
                            root.record("otel.status_code", "error");
                        }
                    }
                    self.set_clock_status(status);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Files;
    use crate::clock::SystemClock;
    use crate::db::test_db;
    use crate::mcp::testing::app_with;
    use crate::okf::workspace::StoredFile;
    use crate::store::audit::AuditActor;
    use crate::store::import::{import, ImportOptions, ImportScope};
    use crate::store::keys as key_rows;
    use chrono::Utc;
    use std::time::{Duration, Instant};

    async fn import_files(pool: &PgPool, catalog: &str, files: &[(&str, &str)], replace: bool) {
        let files: Files = files
            .iter()
            .map(|(path, text)| {
                let file = StoredFile {
                    content: text.as_bytes().to_vec(),
                    executable: false,
                };
                (path.to_string(), file)
            })
            .collect();
        let options = ImportOptions {
            replace,
            scope: ImportScope::Everything,
            expect_generation: None,
            expect_catalog_id: None,
            actor: "tester",
            audit: &crate::store::audit::AuditActor::cli(),
            now: Utc::now(),
        };
        import(pool, catalog, files, &options).await.unwrap();
    }

    async fn new_key(pool: &PgPool, id: &str) -> String {
        key_rows::insert_legacy_test_key(pool, id, key_rows::Access::Read, "tester", Utc::now())
            .await
    }

    async fn app(pool: &PgPool) -> Arc<App> {
        App::load(pool.clone(), Config::default(), Arc::new(SystemClock))
            .await
            .unwrap()
    }

    /// A change-feed message as the listener would parse it off the wire.
    fn message(invalidation: crate::invalidate::Invalidation) -> crate::invalidate::Message {
        crate::invalidate::Message {
            invalidation,
            sent_at_ms: Utc::now().timestamp_millis(),
        }
    }

    async fn create_pg(pool: &PgPool) -> i64 {
        catalogs::create(
            pool,
            &crate::store::catalogs::NewCatalog {
                name: "pg",
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        catalogs::list(pool).await.unwrap()[0].id
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_invalidation_refreshes_that_catalog() {
        use crate::invalidate::Invalidation;
        let db = test_db().await;
        let id = create_pg(&db.pool).await;
        let app = app(&db.pool).await;
        let before = app
            .catalog("pg")
            .await
            .unwrap()
            .unwrap()
            .current()
            .generation();

        // Written on the pool directly, as another replica would.
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        let generation = catalogs::list(&db.pool).await.unwrap()[0].generation;
        assert!(generation > before);
        assert_eq!(
            app.catalog("pg")
                .await
                .unwrap()
                .unwrap()
                .current()
                .generation(),
            before
        );

        app.apply(&message(Invalidation::Catalog { id, generation }))
            .await;

        assert_eq!(
            app.catalog("pg")
                .await
                .unwrap()
                .unwrap()
                .current()
                .generation(),
            generation
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn change_feed_messages_reach_the_catalog_directory() {
        use crate::invalidate::Invalidation;
        let db = test_db().await;
        let id = create_pg(&db.pool).await;
        let app = app(&db.pool).await;
        assert!(app.directory().resolve("pg").await.unwrap().is_some());
        assert!(app.directory().resolve("later").await.unwrap().is_none());
        sqlx::query("UPDATE catalogs SET description = 'second' WHERE id = $1")
            .bind(id)
            .execute(&db.pool)
            .await
            .unwrap();

        app.apply(&message(Invalidation::CatalogMeta {
            id,
            name: "pg".to_owned(),
        }))
        .await;
        let row = app.directory().resolve("pg").await.unwrap().unwrap();
        assert_eq!(row.description, "second");

        // A lost connection forgets the remembered absence as well.
        sqlx::query("INSERT INTO catalogs (name, created_at) VALUES ('later', now())")
            .execute(&db.pool)
            .await
            .unwrap();
        assert!(app.directory().resolve("later").await.unwrap().is_none());
        app.apply_reconnect().await;
        assert!(app.directory().resolve("later").await.unwrap().is_some());
    }

    /// Every store write a directory row reflects (details, maintenance,
    /// suspension, owner, delete) sends a message that makes the directory
    /// reread it: each write's own messages, applied here as the listener
    /// would, and nothing else.
    #[tokio::test(flavor = "multi_thread")]
    async fn each_catalog_writers_messages_refresh_its_directory_row() {
        use crate::invalidate::testing::invalidations_of;
        use crate::store::audit::AuditActor;
        use crate::store::catalogs::{DetailsChange, MaintenanceChange, Switch};
        use crate::store::teams;
        let db = test_db().await;
        create_pg(&db.pool).await;
        let app = app(&db.pool).await;
        let cli = AuditActor::cli();
        let row = || async { app.directory().resolve("pg").await.unwrap() };
        // Applies what `write` sent.
        let applied = |sent: Vec<crate::invalidate::Invalidation>| async {
            for invalidation in sent {
                app.apply(&message(invalidation)).await;
            }
        };
        assert!(row().await.is_some_and(|row| !row.maintenance_off));

        let details = DetailsChange {
            catalog: "pg",
            display_name: None,
            description: Some("Notes."),
            owner: None,
        };
        applied(
            invalidations_of(&db.pool, async {
                catalogs::update_details(&db.pool, &details, &cli, Utc::now())
                    .await
                    .unwrap();
            })
            .await,
        )
        .await;
        assert_eq!(row().await.unwrap().description, "Notes.");

        let off = MaintenanceChange {
            catalog: "pg",
            to: Switch::Off,
            owner: None,
        };
        applied(
            invalidations_of(&db.pool, async {
                catalogs::set_maintenance(&db.pool, &off, &cli, Utc::now())
                    .await
                    .unwrap();
            })
            .await,
        )
        .await;
        assert!(row().await.unwrap().maintenance_off);

        applied(
            invalidations_of(&db.pool, async {
                teams::suspend(&db.pool, crate::db::TEST_TEAM, None, &cli, Utc::now())
                    .await
                    .unwrap();
            })
            .await,
        )
        .await;
        assert!(row().await.unwrap().suspended.is_some());

        let other = crate::store::users::NewUser {
            name: "bob",
            display_name: &crate::labels::DisplayName::default(),
        };
        crate::store::users::create(&db.pool, &other, &cli, Utc::now())
            .await
            .unwrap();
        applied(
            invalidations_of(&db.pool, async {
                catalogs::set_owner(&db.pool, "pg", "bob", &cli, Utc::now())
                    .await
                    .unwrap();
            })
            .await,
        )
        .await;
        let moved = row().await.unwrap();
        assert_eq!(moved.owner_team.as_deref(), Some("bob"));
        assert!(moved.suspended.is_none(), "bob's team is not suspended");

        applied(
            invalidations_of(&db.pool, async {
                catalogs::delete(&db.pool, "pg", false, &cli, Utc::now())
                    .await
                    .unwrap();
            })
            .await,
        )
        .await;
        assert!(row().await.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_credential_invalidation_reloads_the_key_cache() {
        use crate::invalidate::Invalidation;
        let db = test_db().await;
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;
        assert!(app.keys().find(&key).is_some());

        key_rows::revoke(
            &db.pool,
            "reader",
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        // Still cached: nothing told this replica.
        assert!(app.keys().find(&key).is_some());

        app.apply(&message(Invalidation::Credential {
            key_id: "reader".to_owned(),
        }))
        .await;

        assert!(app.keys().find(&key).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_lost_connection_marks_everything_stale() {
        let db = test_db().await;
        create_pg(&db.pool).await;
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;
        let before = app
            .catalog("pg")
            .await
            .unwrap()
            .unwrap()
            .current()
            .generation();

        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        key_rows::revoke(
            &db.pool,
            "reader",
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();

        // The directory is marked stale: the next use reads the row again
        // and refreshes the cached snapshot to it.
        app.apply_reconnect().await;

        assert!(
            app.catalog("pg")
                .await
                .unwrap()
                .unwrap()
                .current()
                .generation()
                > before
        );
        assert!(app.keys().find(&key).is_none());
    }

    /// Creates catalog `name`, owned by the fixture team.
    async fn create(pool: &PgPool, name: &str) -> catalogs::Catalog {
        catalogs::create(
            pool,
            &crate::store::catalogs::NewCatalog {
                name,
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap()
    }

    /// Runs `write` and applies every change-feed message it sent, as this
    /// replica's listener would.
    async fn applied(app: &App, write: impl std::future::Future<Output = ()>) {
        let sent = crate::invalidate::testing::invalidations_of(app.pool(), write).await;
        for invalidation in sent {
            app.apply(&message(invalidation)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_holds_no_catalog_and_every_live_key() {
        let db = test_db().await;
        create(&db.pool, "pg").await;
        create(&db.pool, "work").await;
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        let key = new_key(&db.pool, "reader").await;

        let app = app(&db.pool).await;

        app.catalog_cache().settle().await;
        assert_eq!(
            app.catalog_cache().cached(),
            0,
            "nothing loaded at start-up"
        );
        assert_eq!(app.keys().find(&key).unwrap().key_id, "reader");
        // Each catalog loads on its first use.
        let pg = app.catalog("pg").await.unwrap().unwrap();
        assert_eq!(pg.current().files(), 1);
        assert!(app.catalog("work").await.unwrap().is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn catalogs_changed_beside_the_server_are_served_after_their_messages() {
        let db = test_db().await;
        create(&db.pool, "pg").await;
        create(&db.pool, "gone").await;
        let app = app(&db.pool).await;
        let pg_before = app.catalog("pg").await.unwrap().unwrap();
        assert!(app.catalog("gone").await.unwrap().is_some());
        assert!(app.catalog("work").await.unwrap().is_none());

        // Changes made by the subcommands, beside the running server.
        applied(&app, async {
            import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
            create(&db.pool, "work").await;
            catalogs::delete(&db.pool, "gone", false, &AuditActor::cli(), Utc::now())
                .await
                .unwrap();
        })
        .await;

        // The same state, refreshed in place: readers holding it see the new snapshot.
        let pg_after = app.catalog("pg").await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&pg_before, &pg_after));
        assert_eq!(pg_after.current().files(), 1);
        assert!(app.catalog("work").await.unwrap().is_some());
        assert!(app.catalog("gone").await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_notices_revoked_keys() {
        let db = test_db().await;
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;

        key_rows::revoke(&db.pool, "reader", &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
        app.poll().await.unwrap();

        assert!(app.keys().find(&key).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_created_again_under_its_name_is_loaded_from_scratch() {
        let db = test_db().await;
        let first = create(&db.pool, "pg").await;
        let app = app(&db.pool).await;
        let old = app.catalog("pg").await.unwrap().unwrap();
        assert_eq!(old.id(), first.id);

        applied(&app, async {
            catalogs::delete(&db.pool, "pg", false, &AuditActor::cli(), Utc::now())
                .await
                .unwrap();
            create(&db.pool, "pg").await;
        })
        .await;

        let pg = app.catalog("pg").await.unwrap().unwrap();
        assert_ne!(pg.id(), first.id);
        assert!(app.catalog_cache().resident(first.id).await.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_re_created_catalog_that_fails_to_load_is_not_served_as_the_old_one() {
        let db = test_db().await;
        create(&db.pool, "pg").await;
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        let app = app(&db.pool).await;
        assert!(app.catalog("pg").await.unwrap().is_some());

        applied(&app, async {
            catalogs::delete(&db.pool, "pg", false, &AuditActor::cli(), Utc::now())
                .await
                .unwrap();
            let again = create(&db.pool, "pg").await;
            // A row no workspace accepts (a path escaping the catalog).
            sqlx::query(
                "INSERT INTO documents (catalog_id, path, content, executable, revision, \
                                        updated_at, updated_by) \
                 VALUES ($1, '../escape.md', '', false, 1, now(), 'test')",
            )
            .bind(again.id)
            .execute(&db.pool)
            .await
            .unwrap();
        })
        .await;

        assert!(
            app.catalog("pg").await.unwrap().is_none(),
            "the deleted catalog is not served"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn overlapping_polls_leave_the_newest_key_list() {
        let db = test_db().await;
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;

        key_rows::revoke(
            &db.pool,
            "reader",
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        // `join!` runs both passes concurrently; `polling` serializes them.
        let (first, second) = tokio::join!(app.poll(), app.poll());

        first.unwrap();
        second.unwrap();
        assert!(app.keys().find(&key).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_key_reload_is_retried_at_the_next_pass() {
        let db = test_db().await;
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;
        key_rows::revoke(
            &db.pool,
            "reader",
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        // With the keys table out of the way, the reload fails.
        sqlx::query("ALTER TABLE api_keys RENAME TO api_keys_away")
            .execute(&db.pool)
            .await
            .unwrap();

        app.poll().await.unwrap();
        assert!(app.keys().find(&key).is_some(), "the old cache stays");
        sqlx::query("ALTER TABLE api_keys_away RENAME TO api_keys")
            .execute(&db.pool)
            .await
            .unwrap();
        // No admin change since: only the unstored counter makes this retry.
        app.poll().await.unwrap();

        assert!(app.keys().find(&key).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_admin_generation_read_does_not_stop_the_pass() {
        let db = test_db().await;
        let app = app(&db.pool).await;
        // With the counter's table out of the way, reading it fails.
        sqlx::query("ALTER TABLE server_state RENAME TO server_state_away")
            .execute(&db.pool)
            .await
            .unwrap();

        // Logged, not returned: the pass still completes.
        let passed = app.poll().await;

        assert!(passed.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_settings_change_reaches_the_running_app_at_the_next_poll() {
        let db = test_db().await;
        let app = app(&db.pool).await;
        assert_eq!(app.settings().max_keys_per_user, 20);

        settings::set(
            &db.pool,
            settings::SettingKey::MaxKeysPerUser,
            "5",
            &audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let before_poll = app.settings().max_keys_per_user;
        app.poll().await.unwrap();
        let after_set = app.settings().max_keys_per_user;
        settings::reset(
            &db.pool,
            settings::SettingKey::MaxKeysPerUser,
            &audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        app.poll().await.unwrap();

        assert_eq!(before_poll, 20, "read from memory, not per call");
        assert_eq!(after_set, 5);
        assert_eq!(app.settings().max_keys_per_user, 20, "reset reaches it too");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unreadable_stored_setting_never_stops_the_server() {
        let db = test_db().await;
        // A value a newer server (or a hand edit) wrote that this one cannot
        // read, there before the server starts...
        sqlx::query(
            "INSERT INTO server_settings (key, value, updated_at, updated_by) \
             VALUES ('console_session_idle', '\"forever\"', now(), 'hand')",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        let app = app(&db.pool).await;
        let at_start = app.settings().console_session_idle;
        // ...and one written while it runs, followed by a generation bump.
        sqlx::query(
            "INSERT INTO server_settings (key, value, updated_at, updated_by) \
             VALUES ('max_keys_per_user', '-3', now(), 'hand')",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        settings::set(
            &db.pool,
            settings::SettingKey::PasswordSignIn,
            "off",
            &audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        app.poll().await.unwrap();

        let current = app.settings();
        assert_eq!(at_start, crate::config::DEFAULT_CONSOLE_SESSION_IDLE);
        assert_eq!(
            current.console_session_idle,
            crate::config::DEFAULT_CONSOLE_SESSION_IDLE
        );
        assert_eq!(current.max_keys_per_user, 20);
        assert!(
            !current.password_sign_in,
            "the readable change still applies"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_settings_reload_keeps_the_old_settings_and_retries() {
        let db = test_db().await;
        let app = app(&db.pool).await;
        settings::set(
            &db.pool,
            settings::SettingKey::MaxKeysPerUser,
            "5",
            &audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        sqlx::query("ALTER TABLE server_settings RENAME TO server_settings_away")
            .execute(&db.pool)
            .await
            .unwrap();

        app.poll().await.unwrap();
        let while_unreadable = app.settings().max_keys_per_user;
        sqlx::query("ALTER TABLE server_settings_away RENAME TO server_settings")
            .execute(&db.pool)
            .await
            .unwrap();
        // No new bump: the counter was not stored, so this pass reloads.
        app.poll().await.unwrap();

        assert_eq!(while_unreadable, 20);
        assert_eq!(app.settings().max_keys_per_user, 5);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_until_stops_when_cancelled() {
        // Generous bounds: the tests share a loaded machine, and each pass
        // talks to Postgres. The loop needs only one pass to succeed.
        const PATIENCE: Duration = Duration::from_secs(30);
        let db = test_db().await;
        let config = Config {
            poll_interval: Duration::from_millis(20),
            ..Config::default()
        };
        let app = App::load(db.pool.clone(), config, Arc::new(SystemClock))
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(Arc::clone(&app).poll_until(shutdown.clone()));
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "pg",
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();

        // Within a few intervals the loop has picked the new catalog up.
        let started = Instant::now();
        while app.catalog("pg").await.unwrap().is_none() && started.elapsed() < PATIENCE {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let seen = app.catalog("pg").await.unwrap().is_some();
        shutdown.cancel();

        assert!(seen, "the poll loop never loaded the new catalog");
        tokio::time::timeout(PATIENCE, task)
            .await
            .expect("poll_until returns after cancel")
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_clock_check_runs_at_startup_and_stops_with_the_server() {
        // `None`: the server stamps its reply when the request arrives, after
        // `app_with` has started its database (several seconds).
        let server = crate::ntp::testing::fake_server(None, 1).await;
        let config = Config {
            time_server: Some(server.to_string()),
            ..Config::default()
        };
        let test = app_with(&[("pg", &[])], config).await;
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(Arc::clone(&test.app).check_clock_until(shutdown.clone()));

        // Wait for the startup check, but not forever.
        let status = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(status) = test.app.clock_status() {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a clock check within 10 s");
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the loop stops when cancelled")
            .expect("the loop does not panic");

        assert!(status.ok, "{status:?}");
        // Both ends read the system clock: the offset is a few ms at most.
        assert!(
            status.offset_ms.is_some_and(|ms| ms.abs() < 1_000),
            "{status:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_a_time_server_there_is_no_check() {
        let test = app_with(&[("pg", &[])], Config::default()).await;

        // Returns at once, without a cancelled token.
        tokio::time::timeout(
            Duration::from_secs(1),
            Arc::clone(&test.app).check_clock_until(CancellationToken::new()),
        )
        .await
        .expect("returns at once without a time_server");

        assert_eq!(test.app.clock_status(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pool_gauges() {
        let (test, registry) =
            crate::mcp::testing::app_with_metrics(&[("pg", &[])], Config::default()).await;
        // One connection held out of the pool, so `used` is not zero.
        let _held = test.app.pool().acquire().await.expect("a connection");

        let text = crate::mcp::testing::render(&registry);

        let sample = |state: &str| {
            crate::mcp::testing::sample_of(
                &text,
                &format!(r#"db_client_connection_count{{state="{state}"}}"#),
            )
            .unwrap_or_else(|| panic!("no {state} series: {text}"))
        };
        let (idle, used) = (sample("idle"), sample("used"));
        assert!(used >= 1.0, "{text}");
        assert_eq!(idle + used, f64::from(test.app.pool().size()), "{text}");
    }

    #[tokio::test]
    async fn each_clock_check_is_its_own_trace() {
        use crate::telemetry::config::LogFormat;
        use crate::telemetry::subscriber::capture::Observer;
        for filter in ["info", "trace"] {
            // The fake server answers once, so each filter gets its own.
            let server = crate::ntp::testing::fake_server(None, 1).await;
            let config = Config {
                time_server: Some(server.to_string()),
                ..Config::default()
            };
            let (test, registry) =
                crate::mcp::testing::app_with_metrics(&[("pg", &[])], config).await;
            let observer = Observer::start("off", filter, "off", LogFormat::Text);
            let shutdown = CancellationToken::new();
            let running = Arc::clone(&test.app)
                .check_clock_until(shutdown.clone())
                .instrument(tracing::info_span!("caller"));
            // Stop the loop once its startup check has answered.
            let stop = async {
                // `checked_at` has one-second resolution, so count the
                // checks the metric saw instead.
                let counted = || {
                    crate::mcp::testing::sample_of(
                        &crate::mcp::testing::render(&registry),
                        r#"okf_clock_checks_total{outcome="ok"}"#,
                    )
                    .unwrap_or(0.0)
                };
                let before = counted();
                tokio::time::timeout(Duration::from_secs(10), async {
                    while counted() <= before {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .expect("a clock check within 10 s");
                shutdown.cancel();
            };
            tokio::time::timeout(Duration::from_secs(15), async {
                tokio::join!(running, stop)
            })
            .await
            .expect("the loop stops when cancelled");
            let observed = observer.finish();

            let caller = &observed.spans_named("caller")[0];
            let checks = observed.spans_named("okf.clock.check");
            assert_eq!(checks.len(), 1, "{filter}");
            assert_eq!(
                checks[0].parent_span_id,
                opentelemetry::trace::SpanId::INVALID
            );
            assert_ne!(
                checks[0].span_context.trace_id(),
                caller.span_context.trace_id(),
                "{filter}"
            );
            // The result is on the root: a closed outcome and the offset in
            // seconds as a number, never a string; a success is not an error.
            let attribute = |key| crate::telemetry::subscriber::capture::attribute(checks[0], key);
            assert_eq!(
                attribute("okf.outcome").map(ToString::to_string).as_deref(),
                Some("ok"),
                "{filter}"
            );
            assert!(
                matches!(attribute("okf.clock.offset"), Some(opentelemetry::Value::F64(offset)) if offset.abs() < 1.0),
                "{filter}: {:?}",
                checks[0].attributes
            );
            assert_eq!(
                checks[0].status,
                opentelemetry::trace::Status::Unset,
                "{filter}"
            );
            let text = crate::mcp::testing::render(&registry);
            assert_eq!(
                crate::mcp::testing::sample_of(&text, r#"okf_clock_checks_total{outcome="ok"}"#),
                Some(1.0),
                "{text}"
            );
            let offset = crate::mcp::testing::sample_of(&text, "okf_clock_offset_seconds")
                .unwrap_or_else(|| panic!("no offset gauge: {text}"));
            assert!(offset.abs() < 1.0, "{text}");
        }
    }

    /// A UDP server that answers one request with a packet too short to be
    /// an NTP reply.
    async fn garbage_time_server() -> std::net::SocketAddr {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = socket.local_addr().expect("address");
        tokio::spawn(async move {
            let mut request = [0u8; 48];
            if let Ok((_, client)) = socket.recv_from(&mut request).await {
                let _ = socket.send_to(&[0u8; 4], client).await;
            }
        });
        address
    }

    #[tokio::test]
    async fn a_failed_clock_check_marks_its_root_span_as_an_error() {
        use crate::telemetry::config::LogFormat;
        use crate::telemetry::subscriber::capture::{attribute, Observer};
        for filter in ["info", "trace"] {
            let server = garbage_time_server().await;
            let config = Config {
                time_server: Some(server.to_string()),
                ..Config::default()
            };
            let (test, registry) =
                crate::mcp::testing::app_with_metrics(&[("pg", &[])], config).await;
            let observer = Observer::start("off", filter, "off", LogFormat::Text);
            let shutdown = CancellationToken::new();
            let running = Arc::clone(&test.app).check_clock_until(shutdown.clone());
            let stop = async {
                let app = &test.app;
                tokio::time::timeout(Duration::from_secs(10), async {
                    while app.clock_status().is_none() {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .expect("a clock check within 10 s");
                shutdown.cancel();
            };
            tokio::time::timeout(Duration::from_secs(15), async {
                tokio::join!(running, stop)
            })
            .await
            .expect("the loop stops when cancelled");
            let observed = observer.finish();

            let checks = observed.spans_named("okf.clock.check");
            assert_eq!(checks.len(), 1, "{filter}");
            assert_eq!(
                attribute(checks[0], "okf.outcome")
                    .map(ToString::to_string)
                    .as_deref(),
                Some("error"),
                "{filter}"
            );
            assert!(
                attribute(checks[0], "okf.clock.offset").is_none(),
                "{filter}"
            );
            assert!(
                matches!(checks[0].status, opentelemetry::trace::Status::Error { .. }),
                "{filter}: {:?}",
                checks[0].status
            );
            let text = crate::mcp::testing::render(&registry);
            assert_eq!(
                crate::mcp::testing::sample_of(&text, r#"okf_clock_checks_total{outcome="error"}"#),
                Some(1.0),
                "{filter}: {text}"
            );
        }
    }
}

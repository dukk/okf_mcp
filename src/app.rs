//! The server's shared state (design "Architecture"): the database, the
//! configuration, the clock, the credential caches and every catalog held in
//! memory, the poll loop that notices changes made outside the server
//! (design "Noticing changes made outside the server"), and the clock check
//! loop and its last result (design "Time").

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};

use sqlx::PgPool;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::auth::{Admission, Principal};
use crate::cache::credentials::{AccessView, CredentialLimits, Credentials};
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
use crate::labels::directory::Labels;
use crate::oauth::OAuth;
use crate::oidc::OidcProviders;
use crate::overlay::Overlays;
use crate::shared;
use crate::store::audit::{self, AccessKind, AccessRecord};
use crate::store::catalogs::CatalogGeneration;
use crate::store::settings::{self, Settings};
use crate::store::{catalogs, unlocks};
use crate::telemetry::metrics::{AnnouncementResult, Metrics, Outcome, RefreshResult};
use crate::telemetry::subscriber::{split_event, LOCAL_ONLY};

/// Every loaded catalog, by name (a `BTreeMap`, so iteration is in name
/// order, like `catalog list`).
pub type Catalogs = BTreeMap<String, Arc<CatalogState>>;

/// Catalogs whose stored data failed to build into a snapshot: each one's id,
/// and the generation that failed. Building the same generation again would
/// fail the same way, so it is retried only once the generation moves.
type Broken = HashMap<i64, i64>;

/// The state every route and tool shares. Built once by [`App::load`] and
/// shared as an `Arc<App>`.
pub struct App {
    pool: PgPool,
    config: Config,
    // `Arc<dyn Clock>`: any clock (the real one, or a fixed one in tests),
    // shared between threads. `dyn Clock` is a trait object: Python's duck
    // typing, with the method looked up at run time.
    clock: Arc<dyn Clock>,
    // Keys, grants and users, loaded on demand (M18 Decision 9).
    credentials: Credentials,
    // Every user's, team's and catalog's label, by slug: reloaded with the
    // settings when `admin_generation` moves (until Task 12 of milestone 18
    // loads labels on demand). As with `catalogs`, the lock is held only to
    // clone or swap the `Arc`.
    labels: RwLock<Arc<Labels>>,
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
    // Catalog rows by name with their access facts (M18 Decision 6). Nothing
    // reads it yet but tests: later tasks move the map's readers onto it.
    directory: CatalogDirectory,
    // As in `CatalogState`: the lock is held only to clone or swap the `Arc`.
    catalogs: RwLock<Arc<Catalogs>>,
    // The `admin_generation` the settings and labels were loaded at. Written only under
    // `polling`, so `Relaxed` ordering is enough.
    admin_generation: AtomicI64,
    // The last `admin_generation` this instance published over Redis (or the
    // one it started at). Tracked apart from `admin_generation` so a reload
    // that only loaded a change (and did not announce) cannot make the next
    // announcing reload skip it. Written only under `polling`.
    announced_generation: AtomicI64,
    // The effective sign-in policy (`[auth]` with `server_settings`
    // overrides), reloaded with the labels whenever `admin_generation`
    // moves. As with `catalogs`, the lock is held only to clone or swap.
    settings: RwLock<Arc<Settings>>,
    // Held for a whole `poll` pass. Without it, two overlapping passes (the
    // loop and a test's or a later milestone's direct call) could swap older
    // settings in after newer ones while storing the newer counter.
    // tokio's `Mutex`: it is held across `.await`s. It also guards the
    // pass's own memory, the broken catalogs: a Rust mutex owns the data it
    // protects, which can only be reached through the lock.
    polling: tokio::sync::Mutex<Broken>,
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
    /// Loads the key cache, the effective settings and every catalog's
    /// snapshot.
    ///
    /// A catalog whose snapshot fails to build is logged and left out, as
    /// [`App::poll`] does, and a later poll tries it again (see there for
    /// when): one bad catalog never takes the others offline.
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
    /// [`OkfError::Database`] reading the counter, the keys, the settings or
    /// the list;
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
        // Read the counter before the settings and labels: a change committed
        // between them moves it again, so the next poll reloads.
        let admin_generation = catalogs::admin_generation(&pool).await?;
        let settings = settings::load(&pool, &config.auth).await?;
        let labels = Labels::load(&pool).await?;
        // `clock` moves into `Self` below, so the caches get their own handle.
        let credentials = Credentials::new(
            pool.clone(),
            Arc::clone(&clock),
            CredentialLimits {
                credentials: config.credential_cache_entries,
                users: config.user_cache_entries,
                ttl: config.credential_ttl,
            },
            metrics.clone(),
        );
        let mut loaded = Catalogs::new();
        let mut broken = Broken::new();
        for row in catalogs::list(&pool).await? {
            match CatalogState::load(&pool, &row.name).await {
                Ok(state) => {
                    loaded.insert(row.name, Arc::new(state));
                }
                // Not in the map, so a later poll tries to load it again
                // (unless it was deleted meanwhile).
                Err(error) => note_failure(&mut broken, &row, "load", &error),
            }
        }
        // `if let Some(name) = ...`: Python's `if (name := ...) is not None`.
        if let Some(name) = &config.default_catalog {
            if !loaded.contains_key(name) {
                tracing::warn!(catalog = %name, "default_catalog is not loaded (yet)");
            }
        }
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
        let app = Arc::new(Self {
            pool,
            config,
            clock,
            credentials,
            labels: RwLock::new(Arc::new(labels)),
            sessions,
            redis,
            overlays,
            write_queue: KeyedLocks::new(),
            publish_locks: KeyedLocks::new(),
            rebuild_locks: KeyedLocks::new(),
            directory,
            catalogs: RwLock::new(Arc::new(loaded)),
            admin_generation: AtomicI64::new(admin_generation),
            announced_generation: AtomicI64::new(admin_generation),
            settings: RwLock::new(Arc::new(settings)),
            polling: tokio::sync::Mutex::new(broken),
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
        app.metrics.observe_catalogs(Arc::downgrade(&app));
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

    /// The credential and user caches (M18 Decision 9).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    /// The label directory, as of the last reload.
    ///
    /// # Returns
    /// The shared directory (a pointer copy, not a copy of the maps).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn labels(&self) -> Arc<Labels> {
        Arc::clone(&self.labels.read().expect("labels lock poisoned"))
    }

    /// What every principal of a request is assembled with besides its
    /// credential and user: the `require_unlock` setting and the labels.
    ///
    /// # Returns
    /// The view (two pointer copies).
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn access_view(&self) -> AccessView {
        AccessView {
            require_unlock: self.settings().require_unlock,
            labels: self.labels(),
        }
    }

    /// The principal of a presented API key ([`Credentials::find`]).
    ///
    /// # Arguments
    /// * `presented` - the key from the request.
    ///
    /// # Returns
    /// Its principal, or `None` for an unknown, revoked or expired key.
    ///
    /// # Errors
    /// [`OkfError::Database`] when its load fails; [`OkfError::Busy`] for a
    /// lookup that shared another's failed load.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn find_key(&self, presented: &str) -> Result<Option<Arc<Principal>>, OkfError> {
        self.credentials.find(presented, &self.access_view()).await
    }

    /// A live OAuth grant's principal ([`Credentials::find_grant`]).
    ///
    /// # Arguments
    /// * `grant_id` - the grant's id.
    ///
    /// # Returns
    /// Its principal, or `None` for an unknown, revoked or expired grant.
    ///
    /// # Errors
    /// As [`App::find_key`].
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn find_grant(&self, grant_id: &str) -> Result<Option<Arc<Principal>>, OkfError> {
        self.credentials
            .find_grant(grant_id, &self.access_view())
            .await
    }

    /// The principal of a console session's user
    /// ([`Credentials::session_principal`]).
    ///
    /// # Arguments
    /// * `user_id` - the signed-in user's id.
    ///
    /// # Returns
    /// The principal, or `None` once the user is gone.
    ///
    /// # Errors
    /// As [`App::find_key`].
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn session_principal(&self, user_id: &str) -> Result<Option<Principal>, OkfError> {
        self.credentials
            .session_principal(user_id, &self.access_view())
            .await
    }

    /// The effective sign-in policy, as of the last settings reload. Request
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

    /// Every loaded catalog, as of now. Cheap (an `Arc` clone); one call
    /// gives a consistent view to check names against and take states from.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn catalogs(&self) -> Arc<Catalogs> {
        Arc::clone(&self.catalogs.read().expect("catalog map lock poisoned"))
    }

    /// One catalog's state.
    ///
    /// # Arguments
    /// * `name` - the catalog's name.
    ///
    /// # Returns
    /// Its state, or `None` if no such catalog is loaded.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn catalog(&self, name: &str) -> Option<Arc<CatalogState>> {
        self.catalogs().get(name).cloned()
    }

    /// One pass of the poll loop: reloads the settings and labels if
    /// `admin_generation` moved, and brings the catalogs in line with the
    /// catalog list. Pruning and the cluster counts are housekeeping jobs
    /// ([`crate::housekeeping`]), run once per cluster, not per pass.
    ///
    /// A catalog that fails to load or refresh, or a key reload (or the
    /// `admin_generation` read before it) that fails, is logged and the rest
    /// of the pass goes ahead. A key reload, or a
    /// catalog that failed on a database error, is retried at the next pass.
    /// A catalog whose stored data would not build is retried only once its
    /// `generation` moves (an import or publish), since the same data would
    /// fail the same way: until then a refresh keeps the old snapshot, and a
    /// catalog never loaded stays out. A catalog deleted and created again
    /// under the same name counts as never loaded: if the new one fails to
    /// load, the name is missing until it does, since the old content no
    /// longer exists to serve. Passes never overlap: a call made while another runs waits for
    /// it, then makes its own pass.
    ///
    /// # Returns
    /// `Ok(())` once the pass is done.
    ///
    /// # Errors
    /// [`OkfError::Database`] reading the catalog list.
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
        // The guard releases the mutex when the function returns; through
        // it (`*broken`, like a reference) the pass reads and replaces the
        // broken catalogs.
        let mut broken = self.polling.lock().await;
        if self.reload_settings_if_changed(true).await {
            // `admin_generation` moved and no change message had caught this
            // replica up with it yet (applying one reloads the settings
            // first): something changed that this replica has not been told
            // of, so every cached credential, user and catalog row is
            // reread on its next use. The poll loop's backstop, until it is
            // removed (Task 16).
            self.credentials.mark_all_stale();
            self.directory.mark_all_stale().await;
        }
        let current = self.catalogs();
        let mut next = Catalogs::new();
        // Rebuilt each pass, so a deleted catalog's entry goes with it.
        let mut still_broken = Broken::new();
        for row in catalogs::list(&self.pool).await? {
            // This very generation already failed to build: don't try again.
            let failed_before = broken.get(&row.id) == Some(&row.generation);
            if failed_before {
                still_broken.insert(row.id, row.generation);
            }
            // The same id: the same catalog. A different id under the same
            // name: deleted and created again, so load it from scratch.
            let existing = current.get(&row.name).filter(|state| state.id() == row.id);
            // One span per catalog's turn; its `okf.refresh.result` is set
            // below, and the awaits that do the work run inside it.
            let turn = tracing::info_span!(
                "okf.catalog.refresh",
                okf.catalog = %row.name,
                okf.refresh.result = tracing::field::Empty,
            );
            let mut result = RefreshResult::Unchanged;
            let state = match existing {
                Some(state) => {
                    // Outside the snapshot, so a `catalog update` or
                    // owner change shows without a rebuild.
                    state.set_description(&row.description);
                    state.set_owner_team(row.owner_team.as_deref());
                    let behind = row.generation > state.current().generation();
                    if behind && failed_before {
                        // This generation already failed to build: the turn
                        // is skipped, but it is still a failed one.
                        result = RefreshResult::Failed;
                    } else if behind {
                        match state
                            .refresh(&self.pool, &self.rebuild_locks)
                            .instrument(turn.clone())
                            .await
                        {
                            Ok(true) => result = RefreshResult::Reloaded,
                            Ok(false) => {}
                            Err(error) => {
                                // The old snapshot stays in place.
                                result = RefreshResult::Failed;
                                note_failure(&mut still_broken, &row, "refresh", &error);
                            }
                        }
                    }
                    Arc::clone(state)
                }
                None if failed_before => {
                    // Still failed at this generation: it is a turn too, so
                    // it counts and the span says so.
                    self.metrics
                        .catalog_refresh(&row.name, RefreshResult::Failed);
                    turn.record("okf.refresh.result", RefreshResult::Failed.as_str());
                    continue;
                }
                None => match CatalogState::load(&self.pool, &row.name)
                    .instrument(turn.clone())
                    .await
                {
                    Ok(state) => {
                        tracing::info!(catalog = %row.name, "catalog loaded");
                        result = RefreshResult::Loaded;
                        Arc::new(state)
                    }
                    Err(error) => {
                        note_failure(&mut still_broken, &row, "load", &error);
                        self.metrics
                            .catalog_refresh(&row.name, RefreshResult::Failed);
                        turn.record("okf.refresh.result", RefreshResult::Failed.as_str());
                        continue;
                    }
                },
            };
            self.metrics.catalog_refresh(&row.name, result);
            turn.record("okf.refresh.result", result.as_str());
            next.insert(row.name, state);
        }
        *self.catalogs.write().expect("catalog map lock poisoned") = Arc::new(next);
        *broken = still_broken;
        Ok(())
    }

    /// Reloads the settings and labels if `admin_generation` moved since
    /// they were loaded, serialized with poll passes through `polling`: what
    /// the change feed does for a message that may move them, and the M16
    /// subscriber on a message or a resubscribe.
    ///
    /// # Arguments
    /// * `announce` - publish the loaded generation if it was not announced
    ///   yet: `false` when this reload answers an announcement or a change
    ///   message (the writer already told everyone).
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn reload_settings_now(&self, announce: bool) {
        let _pass = self.polling.lock().await;
        self.reload_settings_if_changed(announce).await;
    }

    /// The `admin_generation` the settings and labels were loaded at.
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

    /// Answers one catalog announcement: refreshes the loaded catalog with
    /// that id unless its snapshot already reached the announced
    /// generation. Each call is the root of its own trace
    /// (`okf.catalog.announcement`) and is counted in
    /// `okf.catalog.announcements`.
    ///
    /// A catalog this replica has not loaded (created since its last poll,
    /// or one that failed to load) is left alone, and so is one deleted
    /// since: the poll loop owns the catalog map and loads or drops it, so
    /// announcements never load anything (no load storm). Concurrent
    /// refreshes of one catalog, from here, the poll loop or a local
    /// publish, are serialized by [`CatalogState::refresh`]'s rebuild lock:
    /// the first rebuilds, the rest see the generation already reached and
    /// return, so a snapshot never moves back and a generation is built
    /// once.
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
        // A linear search: the map is keyed by name, and an announcement
        // names the id, which a catalog created again under the name does not
        // share.
        let state = self
            .catalogs()
            .values()
            .find(|state| state.id() == change.catalog_id)
            .cloned();
        let Some(state) = state else {
            return AnnouncementResult::Unknown;
        };
        span.record("okf.catalog", state.name());
        // Already there (this replica's own publish, the poll loop, or an
        // earlier announcement): not even a database read.
        if change.generation <= state.current().generation() {
            return AnnouncementResult::Current;
        }
        match state.refresh(&self.pool, &self.rebuild_locks).await {
            Ok(true) => AnnouncementResult::Applied,
            Ok(false) => AnnouncementResult::Current,
            // Deleted since this replica's last poll, which drops it.
            Err(OkfError::CatalogNotSelected { .. }) => AnnouncementResult::Unknown,
            Err(error) => {
                split_event!(
                    error,
                    { catalog = %state.name(), kind = error.kind(), },
                    { %error, },
                    "refresh after an announcement failed; the poll loop retries"
                );
                AnnouncementResult::Failed
            }
        }
    }

    /// Applies one change-feed message to this replica's caches: the
    /// catalog directory, the credential and user caches, the catalog map,
    /// and the settings and labels (later tasks of milestone 18 move each of
    /// the last onto its own bounded cache behind this same method).
    ///
    /// A `Catalog` message refreshes that catalog if it is loaded and
    /// behind; a catalog not loaded is left to the poll loop. A
    /// message of any other kind reloads the settings and labels if
    /// `admin_generation` moved (until Tasks 12 and 13).
    ///
    /// # Arguments
    /// * `message` - what changed.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn apply(&self, message: &crate::invalidate::Message) {
        use crate::invalidate::Invalidation;
        // The directory and the credentials first: they are a few map
        // operations, and nothing below should be able to delay dropping a
        // row or a revoked credential.
        self.directory.apply(&message.invalidation).await;
        self.credentials.apply(&message.invalidation).await;
        match &message.invalidation {
            Invalidation::Catalog { id, generation } => {
                self.refresh_announced(
                    CatalogGeneration {
                        catalog_id: *id,
                        generation: *generation,
                    },
                    &tracing::Span::none(),
                )
                .await;
            }
            // A credential message moves `admin_generation` too: reloading
            // keeps this replica at the generation the M16 announcements
            // carry, so they do not mark the credential caches stale.
            Invalidation::Credential { .. }
            | Invalidation::CatalogMeta { .. }
            | Invalidation::User { .. }
            | Invalidation::Team { .. }
            | Invalidation::Settings => self.reload_settings_now(false).await,
        }
    }

    /// After the change feed's connection was lost (or a payload could not
    /// be read): messages sent meanwhile are gone, so every cache is
    /// suspect. The directory, credential and user caches mark every entry
    /// stale; the settings and labels reload; every loaded catalog that is
    /// behind is refreshed.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn apply_reconnect(&self) {
        self.directory.mark_all_stale().await;
        self.credentials.mark_all_stale();
        self.reload_settings_now(false).await;
        self.catch_up_catalogs().await;
    }

    /// What an M16 replica's announcement means here (spec Decision 14):
    /// an older replica changed something without sending change messages,
    /// so every cached credential and user is reread on its next use, and
    /// the settings and labels reload.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn apply_m16_announcement(&self) {
        self.credentials.mark_all_stale();
        self.reload_settings_now(false).await;
    }

    /// Refreshes every loaded catalog whose generation in the database is
    /// newer than its snapshot: what the catalog subscriber does when it
    /// (re)subscribes, since announcements sent while it was not subscribed
    /// are lost. One read of the catalog list, then one
    /// [`App::apply_catalog_announcement`] per catalog that is behind; a
    /// catalog not loaded here is left to the poll loop, as for an
    /// announcement. A failed list read is logged; the poll loop covers it.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn catch_up_catalogs(&self) {
        let rows = match catalogs::list(&self.pool).await {
            Ok(rows) => rows,
            Err(error) => {
                split_event!(
                    error,
                    { kind = error.kind(), },
                    { %error, },
                    "reading the catalog list to catch up failed; the poll loop covers it"
                );
                return;
            }
        };
        let loaded = self.catalogs();
        for row in rows {
            let behind = loaded
                .get(&row.name)
                .filter(|state| state.id() == row.id)
                .is_some_and(|state| row.generation > state.current().generation());
            if behind {
                self.apply_catalog_announcement(CatalogGeneration {
                    catalog_id: row.id,
                    generation: row.generation,
                })
                .await;
            }
        }
    }

    /// Reloads the settings and labels if `admin_generation` moved
    /// since they were loaded, then, when `announce` is set, publishes the
    /// loaded generation if it differs from the last one announced.
    /// Called only under `polling`.
    ///
    /// Failing to read the counter or to reload is logged, not returned: the
    /// stored counter stays as it was, so the next pass tries again, and the
    /// caller's catalog changes go ahead regardless.
    ///
    /// Announcing is tracked apart from loading. A reload that loads a change
    /// without announcing it (the subscriber's resubscribe, a change message)
    /// must not make the `settle` poll that follows find nothing to publish,
    /// so the announce step runs whether or not this call reloaded.
    ///
    /// # Returns
    /// Whether this call reloaded them.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn reload_settings_if_changed(&self, announce: bool) -> bool {
        let reloaded = self.reload_moved_settings().await;
        if !announce {
            return reloaded;
        }
        let Some(redis) = &self.redis else {
            return reloaded;
        };
        let loaded = self.admin_generation.load(Ordering::Relaxed);
        // Under `polling`, so no other reload moves either counter meanwhile.
        if loaded != self.announced_generation.load(Ordering::Relaxed) {
            shared::announce::publish(redis, Some(loaded)).await;
            // Stored even if the publish failed: the failure is counted, and
            // the poll loop covers a lost announcement (spec Decision 12).
            self.announced_generation.store(loaded, Ordering::Relaxed);
        }
        reloaded
    }

    /// The reload half of [`App::reload_settings_if_changed`]. Called only
    /// under `polling`.
    ///
    /// # Returns
    /// Whether `admin_generation` had moved and both were reloaded.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn reload_moved_settings(&self) -> bool {
        let admin_generation = match catalogs::admin_generation(&self.pool).await {
            Ok(admin_generation) => admin_generation,
            Err(error) => {
                split_event!(
                    error,
                    { kind = error.kind(), },
                    { %error, },
                    "reading admin_generation failed; retrying at the next poll"
                );
                return false;
            }
        };
        if admin_generation == self.admin_generation.load(Ordering::Relaxed) {
            return false;
        }
        // An async block lets `?` stop at the first failure; either way the
        // counter is stored only when both were reloaded, so a failure is
        // retried at the next pass. A stored setting this server cannot
        // read is not a failure: `settings::load` falls back to the default.
        // Both are read before either is swapped in, so a failure leaves the
        // pair as it was. The span and metric keep the key cache's name
        // (`okf.keycache.*`), which dashboards know them by.
        let reloaded = async {
            let (settings, labels) = tokio::try_join!(
                settings::load(&self.pool, &self.config.auth),
                Labels::load(&self.pool)
            )?;
            *self.settings.write().expect("settings lock poisoned") = Arc::new(settings);
            *self.labels.write().expect("labels lock poisoned") = Arc::new(labels);
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
            Ok(()) => {
                self.admin_generation
                    .store(admin_generation, Ordering::Relaxed);
                true
            }
            Err(error) => {
                split_event!(
                    error,
                    { kind = error.kind(), },
                    { %error, },
                    "settings or labels reload failed; retrying at the next poll"
                );
                false
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

/// Logs a catalog that failed to load or refresh and, if its stored data is
/// to blame, remembers its generation as broken.
///
/// A catalog deleted since this pass listed it is not a failure: it is gone
/// from the next list, so it is only noted at debug level. A database error
/// is logged but not remembered, so the next pass retries at once.
///
/// # Arguments
/// * `broken` - the broken catalogs, to add to.
/// * `row` - the catalog's row from this pass's list.
/// * `action` - `"load"` or `"refresh"`, for the log line.
/// * `error` - what went wrong.
#[tracing::instrument(level = "trace", skip_all)]
fn note_failure(broken: &mut Broken, row: &catalogs::Catalog, action: &str, error: &OkfError) {
    let retry = match error {
        OkfError::CatalogNotSelected { .. } => {
            tracing::debug!(catalog = %row.name, "{action} skipped: the catalog was deleted");
            return;
        }
        // Transient: the next pass tries again, as for a database error.
        OkfError::Database { .. } | OkfError::Busy { .. } => "at the next poll",
        // Every other error comes from the stored data, so the same
        // generation would fail the same way. (`_` matches any variant.)
        _ => {
            broken.insert(row.id, row.generation);
            "when the catalog changes"
        }
    };
    tracing::error!(
        catalog = %row.name,
        generation = row.generation,
        kind = error.kind(),
        "{action} failed; retrying {retry}"
    );
    tracing::error!(
        target: LOCAL_ONLY,
        catalog = %row.name,
        %error,
        "{action} failed; retrying {retry} (detail)"
    );
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

    fn names(app: &App) -> Vec<String> {
        app.catalogs().keys().cloned().collect()
    }

    /// Stores a row no workspace accepts (a path escaping the catalog), so
    /// the catalog's snapshot fails to build.
    async fn break_catalog(pool: &PgPool, catalog_id: i64) {
        sqlx::query(
            "INSERT INTO documents (catalog_id, path, content, executable, revision, \
                                    updated_at, updated_by) \
             VALUES ($1, '../escape.md', '', false, 1, now(), 'test')",
        )
        .bind(catalog_id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Removes the row [`break_catalog`] stored, without moving the
    /// catalog's generation (as a hand repair in SQL would).
    async fn repair_catalog(pool: &PgPool, catalog_id: i64) {
        sqlx::query("DELETE FROM documents WHERE catalog_id = $1 AND path = '../escape.md'")
            .bind(catalog_id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Moves a catalog's generation, as an import or publish does.
    async fn bump_generation(pool: &PgPool, catalog_id: i64) {
        sqlx::query("UPDATE catalogs SET generation = generation + 1 WHERE id = $1")
            .bind(catalog_id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[test]
    fn only_data_failures_are_remembered_as_broken() {
        let row = catalogs::Catalog {
            id: 7,
            name: "pg".to_string(),
            display_name: String::new(),
            description: String::new(),
            generation: 3,
            created_at: Utc::now(),
            owner_team: None,
        };
        let deleted = OkfError::CatalogNotSelected {
            catalog: "pg".to_string(),
        };
        let database = OkfError::Database {
            source: sqlx::Error::PoolTimedOut,
        };
        let bad_data = OkfError::InvalidPath {
            path: "../escape.md".to_string(),
            reason: "escapes the catalog".to_string(),
        };
        let mut broken = Broken::new();

        note_failure(&mut broken, &row, "load", &deleted);
        note_failure(&mut broken, &row, "load", &database);
        assert!(broken.is_empty());
        note_failure(&mut broken, &row, "refresh", &bad_data);

        assert_eq!(broken, Broken::from([(7, 3)]));
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
        let before = app.catalog("pg").unwrap().current().generation();

        // Written on the pool directly, as another replica would.
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        let generation = catalogs::list(&db.pool).await.unwrap()[0].generation;
        assert!(generation > before);
        assert_eq!(app.catalog("pg").unwrap().current().generation(), before);

        app.apply(&message(Invalidation::Catalog { id, generation }))
            .await;

        assert_eq!(
            app.catalog("pg").unwrap().current().generation(),
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
        assert!(app.find_key(&key).await.unwrap().is_some());

        key_rows::revoke(
            &db.pool,
            "reader",
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        // Still cached: nothing told this replica.
        assert!(app.find_key(&key).await.unwrap().is_some());

        app.apply(&message(Invalidation::Credential {
            key_id: "reader".to_owned(),
        }))
        .await;

        assert!(app.find_key(&key).await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_lost_connection_marks_everything_stale() {
        let db = test_db().await;
        create_pg(&db.pool).await;
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;
        let before = app.catalog("pg").unwrap().current().generation();

        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        key_rows::revoke(
            &db.pool,
            "reader",
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();

        // Today's caches have no stale flag: reloading is the equivalent.
        app.apply_reconnect().await;

        assert!(app.catalog("pg").unwrap().current().generation() > before);
        assert!(app.find_key(&key).await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_holds_every_catalog_and_live_key() {
        let db = test_db().await;
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
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "work",
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        let key = new_key(&db.pool, "reader").await;

        let app = app(&db.pool).await;

        assert_eq!(names(&app), ["pg", "work"]);
        assert_eq!(app.catalog("pg").unwrap().current().files(), 1);
        assert_eq!(app.find_key(&key).await.unwrap().unwrap().key_id, "reader");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_notices_imports_new_and_deleted_catalogs_and_revoked_keys() {
        let db = test_db().await;
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
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "gone",
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;
        let pg_before = app.catalog("pg").unwrap();

        // Changes made by the subcommands, beside the running server.
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "work",
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        catalogs::delete(
            &db.pool,
            "gone",
            false,
            &crate::store::audit::AuditActor::cli(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        key_rows::revoke(
            &db.pool,
            "reader",
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        app.poll().await.unwrap();

        assert_eq!(names(&app), ["pg", "work"]);
        // The same state, refreshed in place: readers holding it see the new snapshot.
        let pg_after = app.catalog("pg").unwrap();
        assert!(Arc::ptr_eq(&pg_before, &pg_after));
        assert_eq!(pg_after.current().files(), 1);
        assert!(app.find_key(&key).await.unwrap().is_none());
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
        assert!(app.find_key(&key).await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_picks_up_a_new_description_without_a_rebuild() {
        let db = test_db().await;
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "pg",
                display_name: &crate::labels::DisplayName::default(),
                description: "copy",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let app = app(&db.pool).await;
        let pg = app.catalog("pg").unwrap();
        let before = pg.current();

        let change = catalogs::DetailsChange {
            catalog: "pg",
            display_name: None,
            description: Some("source of truth"),
            owner: None,
        };
        catalogs::update_details(
            &db.pool,
            &change,
            &crate::store::audit::AuditActor::cli(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        app.poll().await.unwrap();

        assert_eq!(&*pg.description(), "source of truth");
        // `Arc::ptr_eq`: the very same snapshot object, so nothing was rebuilt.
        assert!(Arc::ptr_eq(&before, &app.catalog("pg").unwrap().current()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_picks_up_a_new_owner_without_a_rebuild() {
        let db = test_db().await;
        let alice = crate::store::users::NewUser {
            name: "alice",
            display_name: &crate::labels::DisplayName::default(),
        };
        crate::store::users::create(&db.pool, &alice, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
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
        let app = app(&db.pool).await;
        let pg = app.catalog("pg").unwrap();
        let before = pg.current();
        assert_eq!(pg.owner_team().as_deref(), Some(crate::db::TEST_TEAM));

        catalogs::set_owner(
            &db.pool,
            "pg",
            "alice",
            &crate::store::audit::AuditActor::cli(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        app.poll().await.unwrap();

        assert_eq!(pg.owner_team().as_deref(), Some("alice"));
        assert!(Arc::ptr_eq(&before, &app.catalog("pg").unwrap().current()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_reloads_a_catalog_created_again_under_the_same_name() {
        let db = test_db().await;
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "pg",
                display_name: &crate::labels::DisplayName::default(),
                description: "first",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let app = app(&db.pool).await;
        let first_id = app.catalog("pg").unwrap().id();

        catalogs::delete(
            &db.pool,
            "pg",
            false,
            &crate::store::audit::AuditActor::cli(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "pg",
                display_name: &crate::labels::DisplayName::default(),
                description: "second",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        app.poll().await.unwrap();

        let pg = app.catalog("pg").unwrap();
        assert_ne!(pg.id(), first_id);
        assert_eq!(&*pg.description(), "second");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_that_fails_to_load_is_skipped_until_it_changes() {
        let db = test_db().await;
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
        let bad = catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "bad",
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        break_catalog(&db.pool, bad.id).await;

        // At startup and at a poll alike, the other catalogs go ahead.
        let app = app(&db.pool).await;
        assert_eq!(names(&app), ["pg"]);
        app.poll().await.unwrap();
        assert_eq!(names(&app), ["pg"]);
        // Repaired behind the server's back: the generation it failed at is
        // not built again, so the repair goes unnoticed...
        repair_catalog(&db.pool, bad.id).await;
        app.poll().await.unwrap();
        assert_eq!(names(&app), ["pg"]);
        // ...until the catalog changes, as an import would change it.
        bump_generation(&db.pool, bad.id).await;
        app.poll().await.unwrap();

        assert_eq!(names(&app), ["bad", "pg"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_refresh_keeps_the_old_snapshot_until_the_catalog_changes() {
        let db = test_db().await;
        let pg = catalogs::create(
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
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        let app = app(&db.pool).await;
        break_catalog(&db.pool, pg.id).await;
        bump_generation(&db.pool, pg.id).await;

        app.poll().await.unwrap();
        let loaded = app.catalog("pg").unwrap().current();
        assert_eq!((loaded.generation(), loaded.files()), (1, 1));
        // Not rebuilt at the generation that failed, even once it would build.
        repair_catalog(&db.pool, pg.id).await;
        app.poll().await.unwrap();
        assert_eq!(app.catalog("pg").unwrap().current().generation(), 1);
        bump_generation(&db.pool, pg.id).await;
        app.poll().await.unwrap();

        let loaded = app.catalog("pg").unwrap().current();
        assert_eq!((loaded.generation(), loaded.files()), (3, 1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_that_stays_unloadable_counts_a_failed_refresh_every_pass() {
        let (test, registry) =
            crate::mcp::testing::app_with_metrics(&[], crate::config::Config::default()).await;
        let pool = test.app.pool().clone();
        let pg = catalogs::create(
            &pool,
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
        import_files(&pool, "pg", &[("a.md", "# A\n")], false).await;
        break_catalog(&pool, pg.id).await;

        // The first pass tries the load and fails; the second skips the
        // generation that already failed. Both are turns that failed.
        test.app.poll().await.unwrap();
        test.app.poll().await.unwrap();

        assert!(test.app.catalog("pg").is_none());
        let text = crate::mcp::testing::render(&registry);
        assert!(
            text.contains(r#"okf_catalog_refreshes_total{catalog="pg",result="failed"} 2"#),
            "{text}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_loaded_catalog_that_keeps_failing_to_refresh_counts_failed_every_pass() {
        use crate::telemetry::subscriber::capture::{SeenValue, SpanRecorder};

        let (test, registry) = crate::mcp::testing::app_with_metrics(
            &[("pg", &[("a.md", "# A\n")])],
            crate::config::Config::default(),
        )
        .await;
        let pool = test.app.pool().clone();
        let id = test.app.catalog("pg").unwrap().id();
        break_catalog(&pool, id).await;
        bump_generation(&pool, id).await;
        let recorder = SpanRecorder::new();

        // The first pass tries the refresh and fails; the second skips the
        // generation that already failed. Both are turns that failed.
        recorder.run(async {
            test.app.poll().await.unwrap();
            test.app.poll().await.unwrap();
        });

        // The old snapshot still serves.
        assert!(test.app.catalog("pg").is_some());
        let text = crate::mcp::testing::render(&registry);
        assert!(
            text.contains(r#"okf_catalog_refreshes_total{catalog="pg",result="failed"} 2"#),
            "{text}"
        );
        let results: Vec<_> = recorder
            .finished()
            .iter()
            .filter(|span| span.name == "okf.catalog.refresh")
            .map(|span| span.field("okf.refresh.result").cloned())
            .collect();
        let failed = Some(SeenValue::Text("failed".to_string()));
        assert_eq!(results, [failed.clone(), failed]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_re_created_catalog_that_fails_to_load_drops_the_old_one() {
        let db = test_db().await;
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
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        let app = app(&db.pool).await;
        catalogs::delete(
            &db.pool,
            "pg",
            false,
            &crate::store::audit::AuditActor::cli(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        let again = catalogs::create(
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
        break_catalog(&db.pool, again.id).await;

        app.poll().await.unwrap();

        assert!(
            app.catalog("pg").is_none(),
            "the deleted catalog is not served"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_reload_does_not_stop_the_pass_and_is_retried() {
        let db = test_db().await;
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;
        // Cached before the revocation, whose message this app never hears
        // (lib tests run no listener): only the poll's backstop drops it.
        assert!(app.find_key(&key).await.unwrap().is_some());
        key_rows::revoke(
            &db.pool,
            "reader",
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "work",
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        // With the settings table out of the way, the reload fails.
        sqlx::query("ALTER TABLE server_settings RENAME TO server_settings_away")
            .execute(&db.pool)
            .await
            .unwrap();

        app.poll().await.unwrap();
        assert_eq!(names(&app), ["work"], "the catalog part of the pass ran");
        assert!(
            app.find_key(&key).await.unwrap().is_some(),
            "nothing is marked stale for a reload that failed"
        );
        sqlx::query("ALTER TABLE server_settings_away RENAME TO server_settings")
            .execute(&db.pool)
            .await
            .unwrap();
        // No admin change since: only the unstored counter makes this retry.
        app.poll().await.unwrap();
        assert!(app.find_key(&key).await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_admin_generation_read_does_not_stop_the_pass() {
        let db = test_db().await;
        let app = app(&db.pool).await;
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "work",
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        // With the counter's table out of the way, reading it fails.
        sqlx::query("ALTER TABLE server_state RENAME TO server_state_away")
            .execute(&db.pool)
            .await
            .unwrap();

        app.poll().await.unwrap();

        assert_eq!(names(&app), ["work"]);
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
        while app.catalog("pg").is_none() && started.elapsed() < PATIENCE {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let seen = app.catalog("pg").is_some();
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

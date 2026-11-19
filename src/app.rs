//! The server's shared state (design "Architecture"): the database, the
//! configuration, the clock, the credential caches and every catalog held in
//! memory, the poll loop that notices changes made outside the server
//! (design "Noticing changes made outside the server"), and the clock check
//! loop and its last result (design "Time").

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use sqlx::PgPool;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::auth::{Admission, Principal};
use crate::cache::credentials::{AccessView, CredentialLimits, Credentials};
use crate::cache::directory::CatalogDirectory;
use crate::cache::labels::{LabelLookup, DEFAULT_LABEL_CACHE_ENTRIES};
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

/// The settings as last loaded, with the `admin_generation` they were
/// checked at and when (spec Decision 9): `admin_generation` moves with
/// every administrative change, settings writes included, so an unmoved
/// counter means the loaded settings are still the stored ones.
#[derive(Debug, Clone)]
struct LoadedSettings {
    /// The effective settings.
    settings: Arc<Settings>,
    /// The `admin_generation` read just before they were loaded, or at the
    /// last check that found it unmoved.
    generation: i64,
    /// When the last check began, by the app's clock.
    validated_at: chrono::DateTime<chrono::Utc>,
}

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
    // Labels of the slugs a response shows, read on demand and cached
    // (M18 Decision 9).
    labels: LabelLookup,
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
    // The `admin_generation` the poll pass last marked the credential caches
    // and the catalog directory stale at (or the one this instance started
    // at). The pass reads the counter itself: a change no message described
    // (an older replica's, or a lost message) moves it, and the next pass
    // must see it whatever reloaded meanwhile. Written only under `polling`.
    swept_generation: AtomicI64,
    // The effective sign-in policy (`[auth]` with `server_settings`
    // overrides), with the `admin_generation` it was checked at and when
    // (spec Decision 9). Reloaded on a `Settings` message, and checked again
    // by the first request after `credential_ttl` ([`App::fresh_settings`]).
    // The lock is held only to clone or swap.
    settings: RwLock<LoadedSettings>,
    // Set when the settings must be checked before the next request uses
    // them, whatever their age: after a reconnect or an M16 announcement.
    settings_stale: AtomicBool,
    // Held while the settings are checked or reloaded, so callers that find
    // them expired together run one check (tokio's `Mutex`: it is held
    // across `.await`s).
    settings_reload: tokio::sync::Mutex<()>,
    // How many settings checks have finished, successful or not. A caller
    // that waited for `settings_reload` while one finished takes its result
    // instead of checking again, so during a database outage one failing
    // check serves every request that queued behind it.
    settings_rounds: AtomicU64,
    // How many times the settings were marked stale, and how many marks
    // there had been when the last finished check began. A waiter shares a
    // finished check only when no mark has landed since it began: a newer
    // invalidation needs a check of its own, while the mark a failed check
    // restored for its retry does not.
    settings_marks: AtomicU64,
    checked_marks: AtomicU64,
    // Test counters: how many settings checks, and how many
    // `settings::load`s, ran after start-up.
    #[cfg(test)]
    pub(crate) settings_checks: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(crate) settings_loads: std::sync::atomic::AtomicUsize,
    // Test counter: how many M16 announcements were applied.
    #[cfg(test)]
    pub(crate) m16_announcements: std::sync::atomic::AtomicUsize,
    // Held for a whole `poll` pass. Without it, two overlapping passes (the
    // loop and a test's or a later milestone's direct call) could swap older
    // labels in after newer ones while storing the newer counter.
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
        // Read the counter before the settings: a change committed between
        // them moves it again, so the next check reloads.
        let validated_at = clock.now();
        let admin_generation = catalogs::admin_generation(&pool).await?;
        let settings = settings::load(&pool, &config.auth).await?;
        let labels = LabelLookup::new(
            pool.clone(),
            DEFAULT_LABEL_CACHE_ENTRIES,
            config.credential_ttl,
        )
        .with_metrics(metrics.clone());
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
            labels,
            sessions,
            redis,
            overlays,
            write_queue: KeyedLocks::new(),
            publish_locks: KeyedLocks::new(),
            rebuild_locks: KeyedLocks::new(),
            directory,
            catalogs: RwLock::new(Arc::new(loaded)),
            swept_generation: AtomicI64::new(admin_generation),
            settings: RwLock::new(LoadedSettings {
                settings: Arc::new(settings),
                generation: admin_generation,
                validated_at,
            }),
            settings_stale: AtomicBool::new(false),
            settings_reload: tokio::sync::Mutex::new(()),
            settings_rounds: AtomicU64::new(0),
            settings_marks: AtomicU64::new(0),
            checked_marks: AtomicU64::new(0),
            #[cfg(test)]
            settings_checks: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            settings_loads: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            m16_announcements: std::sync::atomic::AtomicUsize::new(0),
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

    /// The label lookup: the labels of the slugs a response shows, read on
    /// demand ([`LabelLookup::labels_for`]).
    ///
    /// # Returns
    /// The lookup.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn labels(&self) -> &LabelLookup {
        &self.labels
    }

    /// What every principal of a request is assembled with besides its
    /// credential and user: the `require_unlock` setting, checked against
    /// the database when older than `credential_ttl`
    /// ([`App::fresh_settings`]).
    ///
    /// # Returns
    /// The view.
    #[tracing::instrument(level = "trace", skip_all)]
    pub async fn access_view(&self) -> AccessView {
        AccessView {
            require_unlock: self.fresh_settings().await.require_unlock,
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
        self.credentials
            .find(presented, &self.access_view().await)
            .await
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
            .find_grant(grant_id, &self.access_view().await)
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
            .session_principal(user_id, &self.access_view().await)
            .await
    }

    /// The effective sign-in policy, as of the last settings reload. Request
    /// handlers read it here, never from `server_settings`. Every HTTP
    /// request calls [`App::fresh_settings`] first, so what this returns
    /// during a request is never older than `credential_ttl` plus that
    /// request.
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
        Arc::clone(
            &self
                .settings
                .read()
                .expect("settings lock poisoned")
                .settings,
        )
    }

    /// The settings, checked against the database first when they are older
    /// than `credential_ttl` or marked stale (spec Decision 9: the TTL
    /// backstop for a lost `Settings` message). The check reads
    /// `admin_generation` and reloads the settings only if it moved.
    /// Callers that find them expired together run one check: the rest wait
    /// for it and take its result, a failed one included, whatever made it
    /// run (unless the settings were marked stale again after it began). A
    /// request that arrives after a failed check runs a check of its own. A failed check keeps serving the loaded
    /// settings, logs at `warn` and leaves them expired, so the next request
    /// tries again.
    ///
    /// # Returns
    /// The settings (an `Arc` clone).
    #[tracing::instrument(level = "trace", skip_all)]
    pub async fn fresh_settings(&self) -> Arc<Settings> {
        if let Some(settings) = self.settings_if_fresh() {
            return settings;
        }
        let round = self.settings_rounds.load(Ordering::Acquire);
        // The guard is held until it goes out of scope at the end of the
        // function, like a Python `with` block.
        let _reload = self.settings_reload.lock().await;
        // Another caller may have checked them while this one waited.
        if let Some(settings) = self.settings_if_fresh() {
            return settings;
        }
        // That check failed: take its answer rather than queue another
        // failing one, unless a mark landed after it began.
        let checked_meanwhile = self.settings_rounds.load(Ordering::Acquire) != round;
        let marked_since = self.settings_marks.load(Ordering::Acquire)
            != self.checked_marks.load(Ordering::Acquire);
        if checked_meanwhile && !marked_since {
            return self.settings();
        }
        // Read before the check clears the flag: a mark that lands after
        // this counts as newer than the check.
        let marks = self.settings_marks.load(Ordering::Acquire);
        self.revalidate_settings().await;
        self.checked_marks.store(marks, Ordering::Release);
        self.settings_rounds.fetch_add(1, Ordering::Release);
        self.settings()
    }

    /// Marks the settings stale and checks them at once: what a `Settings`
    /// message, a reconnect, an M16 announcement and a poll pass do.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn reload_settings(&self) {
        self.mark_settings_stale();
        self.fresh_settings().await;
    }

    /// Marks the settings for a check before their next use, whatever their
    /// age. The count goes first, so a check that clears the flag after
    /// this has already counted the mark as newer than itself.
    #[tracing::instrument(level = "trace", skip_all)]
    fn mark_settings_stale(&self) {
        self.settings_marks.fetch_add(1, Ordering::AcqRel);
        self.settings_stale.store(true, Ordering::Release);
    }

    /// The loaded settings if they are neither marked stale nor older than
    /// `credential_ttl`. A validation time in the future (the clock stepped
    /// back) counts as expired, so a clock change cannot keep them forever.
    ///
    /// # Returns
    /// The settings, or `None` when they must be checked first.
    #[tracing::instrument(level = "trace", skip_all)]
    fn settings_if_fresh(&self) -> Option<Arc<Settings>> {
        if self.settings_stale.load(Ordering::Acquire) {
            return None;
        }
        let loaded = self.settings.read().expect("settings lock poisoned");
        // `to_std` fails for a negative age.
        let age = self.clock.now().signed_duration_since(loaded.validated_at);
        let fresh = age
            .to_std()
            .is_ok_and(|age| age < self.config.credential_ttl);
        fresh.then(|| Arc::clone(&loaded.settings))
    }

    /// Checks the settings against `admin_generation` and reloads them if it
    /// moved. Called only under `settings_reload`. The counter is read
    /// before the settings, so a change committed between the two reads
    /// moves it again and the next check reloads.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn revalidate_settings(&self) {
        // Cleared before the read, so a mark that lands during it sets the
        // flag again and the next request checks once more.
        let was_stale = self.settings_stale.swap(false, Ordering::AcqRel);
        #[cfg(test)]
        self.settings_checks.fetch_add(1, Ordering::Relaxed);
        let checked_at = self.clock.now();
        let loaded_generation = self
            .settings
            .read()
            .expect("settings lock poisoned")
            .generation;
        let checked = async {
            let generation = catalogs::admin_generation(&self.pool).await?;
            if generation == loaded_generation {
                return Ok(None);
            }
            #[cfg(test)]
            self.settings_loads.fetch_add(1, Ordering::Relaxed);
            // A stored setting this server cannot read is not a failure:
            // `settings::load` falls back to the default.
            // The span and metric keep the key cache's name
            // (`okf.keycache.*`), which dashboards know them by.
            let loaded = settings::load(&self.pool, &self.config.auth)
                .instrument(tracing::info_span!("okf.keycache.reload"))
                .await;
            self.metrics.keycache_reload(if loaded.is_ok() {
                Outcome::Ok
            } else {
                Outcome::Error
            });
            Ok::<_, OkfError>(Some((generation, loaded?)))
        }
        .await;
        match checked {
            Ok(reloaded) => {
                let mut loaded = self.settings.write().expect("settings lock poisoned");
                loaded.validated_at = checked_at;
                if let Some((generation, settings)) = reloaded {
                    loaded.generation = generation;
                    loaded.settings = Arc::new(settings);
                }
            }
            Err(error) => {
                if was_stale {
                    self.settings_stale.store(true, Ordering::Release);
                }
                split_event!(
                    warn,
                    { kind = error.kind(), },
                    { %error, },
                    "settings check failed; serving the loaded settings until the next request retries"
                );
            }
        }
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

    /// The loaded state of the catalog a directory row describes, for a
    /// listing's snapshot fields (files, concepts, last change). M18 Task 8
    /// replaces it with a load through the catalog cache.
    ///
    /// # Arguments
    /// * `row` - the catalog's row, as a listing read it.
    ///
    /// # Returns
    /// Its state, or `None` if this replica has not loaded that catalog, or
    /// holds another incarnation under its name (deleted and created again
    /// since the last poll): a listing never mixes one catalog's row with
    /// another's contents.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn loaded_catalog(&self, row: &catalogs::CatalogRow) -> Option<Arc<CatalogState>> {
        self.catalog(&row.name).filter(|state| state.id() == row.id)
    }

    /// One pass of the poll loop: checks the settings, reloads the labels if
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
        self.reload_settings().await;
        match catalogs::admin_generation(&self.pool).await {
            Ok(generation) => {
                // `admin_generation` moved since the last pass's sweep: a
                // change no message described may be among those, so every
                // cached credential, user, label and catalog row is reread on
                // its next use. The poll loop's backstop, until it is removed
                // (Task 16).
                if generation != self.swept_generation.load(Ordering::Relaxed) {
                    self.credentials.mark_all_stale();
                    self.labels.mark_all_stale();
                    self.directory.mark_all_stale().await;
                    self.swept_generation.store(generation, Ordering::Relaxed);
                }
            }
            Err(error) => {
                split_event!(
                    error,
                    { kind = error.kind(), },
                    { %error, },
                    "reading admin_generation failed; retrying at the next poll"
                );
            }
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

    /// M16 bridge (spec Decision 14): remove in the release after 0.23.
    ///
    /// After an administrative change this replica committed, tells
    /// milestone 16 replicas to check `admin_generation` now
    /// ([`shared::announce::publish_bridge`]); the change itself reached
    /// every milestone 18 replica through the change feed. Best effort and
    /// detached, like [`App::announce_catalog`]: a slow or failing Redis
    /// never delays the caller's answer, and an M16 replica's own poll
    /// covers a lost announcement. Does nothing without `[redis]`.
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn bridge_to_m16(&self) {
        let Some(redis) = self.redis.clone() else {
            return;
        };
        // M16 bridge (spec Decision 14): remove in the release after 0.23.
        tokio::spawn(shared::announce::publish_bridge(redis).instrument(tracing::Span::current()));
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
    /// catalog directory, the credential, user and label caches, the catalog
    /// map and the settings.
    ///
    /// A `Catalog` message refreshes that catalog if it is loaded and
    /// behind; a catalog not loaded is left to the poll loop. A `Settings`
    /// message reloads the settings. The caches apply every other kind
    /// themselves.
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
        self.labels.apply(&message.invalidation).await;
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
            Invalidation::Settings => self.reload_settings().await,
            Invalidation::CatalogMeta { .. }
            | Invalidation::User { .. }
            | Invalidation::Team { .. }
            | Invalidation::Credential { .. } => {}
        }
    }

    /// After the change feed's connection was lost (or a payload could not
    /// be read): messages sent meanwhile are gone, so every cache is
    /// suspect. The directory, credential, user and label caches mark every
    /// entry stale; the settings are checked; every loaded catalog that is
    /// behind is refreshed.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn apply_reconnect(&self) {
        self.directory.mark_all_stale().await;
        self.credentials.mark_all_stale();
        self.labels.mark_all_stale();
        self.reload_settings().await;
        self.catch_up_catalogs().await;
    }

    /// M16 bridge (spec Decision 14): remove in the release after 0.23.
    ///
    /// What an M16 replica's announcement means here: an older replica
    /// changed something without sending change messages, so every cached
    /// credential, user, label and catalog row (suspension, owner,
    /// maintenance) is reread on its next use, and the settings are checked.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn apply_m16_announcement(&self) {
        #[cfg(test)]
        self.m16_announcements.fetch_add(1, Ordering::Relaxed);
        self.credentials.mark_all_stale();
        self.labels.mark_all_stale();
        self.directory.mark_all_stale().await;
        self.reload_settings().await;
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
        set_setting(&db.pool, settings::SettingKey::MaxKeysPerUser, "5").await;
        // With the settings table out of the way, the settings check fails.
        sqlx::query("ALTER TABLE server_settings RENAME TO server_settings_away")
            .execute(&db.pool)
            .await
            .unwrap();

        app.poll().await.unwrap();
        assert_eq!(names(&app), ["work"], "the catalog part of the pass ran");
        assert!(
            app.find_key(&key).await.unwrap().is_none(),
            "the sweep does not wait for the settings"
        );
        assert_eq!(
            app.settings().max_keys_per_user,
            20,
            "the old settings serve"
        );
        sqlx::query("ALTER TABLE server_settings_away RENAME TO server_settings")
            .execute(&db.pool)
            .await
            .unwrap();
        // No admin change since: only the failed check makes this retry.
        app.poll().await.unwrap();
        assert_eq!(app.settings().max_keys_per_user, 5);
    }

    /// An app whose catalog rows and credentials live an hour (so only an
    /// invalidation, never a TTL, can reread them), with `main` owned by
    /// the test team and a writer key of the test user on it, warmed: its
    /// principal assembled and `main`'s row cached.
    async fn warmed_writer() -> (
        crate::db::pg_support::TestDb,
        Arc<App>,
        Arc<crate::auth::Principal>,
    ) {
        let db = test_db().await;
        create_catalog_main(&db.pool).await;
        let names = ["main".to_string()];
        let created = key_rows::create_with_id(
            &db.pool,
            &key_rows::NewKey {
                name: "writer",
                access: key_rows::Access::Write,
                maintenance_report: false,
                grants: key_rows::Grants::Catalogs(&names),
                default_catalog: None,
                actor: "writer",
                user: crate::db::TEST_USER,
                self_issued: false,
                expires_at: None,
                levels: &[],
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let config = Config {
            catalog_ttl: std::time::Duration::from_secs(3600),
            credential_ttl: std::time::Duration::from_secs(3600),
            ..Config::default()
        };
        let app = App::load(db.pool.clone(), config, Arc::new(SystemClock))
            .await
            .unwrap();
        let writer = app.find_key(&created.key).await.unwrap().unwrap();
        assert!(writer
            .require_in(app.directory(), key_rows::Access::Read, "main")
            .await
            .is_ok());
        (db, app, writer)
    }

    async fn create_catalog_main(pool: &PgPool) {
        catalogs::create(
            pool,
            &crate::store::catalogs::NewCatalog {
                name: "main",
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
    }

    /// Suspends the test team as a milestone 16 replica does: the row and
    /// `admin_generation` in one transaction, and no change message.
    async fn suspend_as_an_older_replica(pool: &PgPool) {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(
            "UPDATE teams SET suspended_at = now(), suspension_notice = 'Paused.' WHERE name = $1",
        )
        .bind(crate::db::TEST_TEAM)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query("UPDATE server_state SET admin_generation = admin_generation + 1")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    async fn suspended(app: &App, writer: &crate::auth::Principal) -> bool {
        matches!(
            writer
                .require_in(app.directory(), key_rows::Access::Read, "main")
                .await,
            Err(OkfError::Suspended { .. })
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_older_replicas_announcement_rereads_the_cached_catalog_rows() {
        let (db, app, writer) = warmed_writer().await;
        suspend_as_an_older_replica(&db.pool).await;
        assert!(!suspended(&app, &writer).await, "the row is still cached");

        app.apply_m16_announcement().await;

        assert!(
            suspended(&app, &writer).await,
            "the next check reads it again"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_change_absorbed_by_a_messages_reload_is_still_swept_by_the_next_poll() {
        let (db, app, writer) = warmed_writer().await;
        suspend_as_an_older_replica(&db.pool).await;
        // A change message for something else reloads the settings and
        // catches `admin_generation` up past the silent suspension.
        let me: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(crate::db::TEST_USER)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        app.apply(&message(crate::invalidate::Invalidation::User { id: me }))
            .await;
        assert!(!suspended(&app, &writer).await, "no message named the row");

        app.poll().await.unwrap();

        assert!(
            suspended(&app, &writer).await,
            "the poll's sweep rereads it"
        );
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

    /// A clock a test moves by hand.
    struct MovableClock(std::sync::Mutex<chrono::DateTime<Utc>>);

    impl MovableClock {
        fn advance(&self, by: Duration) {
            let mut now = self.0.lock().expect("clock lock");
            *now += chrono::Duration::from_std(by).expect("a small duration");
        }
    }

    impl Clock for MovableClock {
        #[cfg_attr(
            dylint_lib = "hyp_tracing",
            allow(uninstrumented_fn, reason = "test double")
        )]
        fn now(&self) -> chrono::DateTime<Utc> {
            *self.0.lock().expect("clock lock")
        }
    }

    /// An app on `pool` whose clock the test moves, with the default
    /// `credential_ttl`.
    async fn app_with_movable_clock(pool: &PgPool) -> (Arc<App>, Arc<MovableClock>) {
        let clock = Arc::new(MovableClock(std::sync::Mutex::new(Utc::now())));
        let app = App::load(pool.clone(), Config::default(), clock.clone())
            .await
            .unwrap();
        (app, clock)
    }

    async fn set_setting(pool: &PgPool, key: settings::SettingKey, value: &str) {
        settings::set(pool, key, value, &audit::AuditActor::cli(), Utc::now())
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_settings_invalidation_reloads_the_settings() {
        let db = test_db().await;
        let app = app(&db.pool).await;
        set_setting(&db.pool, settings::SettingKey::MaxKeysPerUser, "5").await;
        let before = app.settings().max_keys_per_user;

        app.apply(&message(crate::invalidate::Invalidation::Settings))
            .await;

        assert_eq!(before, 20, "nothing told this replica yet");
        assert_eq!(app.settings().max_keys_per_user, 5);
    }

    /// The TTL backstop (spec Decision 9): no listener runs in a lib test,
    /// so the setting's `NOTIFY` reaches no one; only the age of the loaded
    /// settings makes the principal's assembly read them again.
    #[tokio::test(flavor = "multi_thread")]
    async fn settings_are_revalidated_after_the_ttl_without_a_notification() {
        let db = test_db().await;
        let key = new_key(&db.pool, "reader").await;
        let (app, clock) = app_with_movable_clock(&db.pool).await;
        let require_unlock = |app: Arc<App>, key: String| async move {
            app.find_key(&key)
                .await
                .unwrap()
                .expect("a live key")
                .facts
                .require_unlock
        };
        assert!(!require_unlock(app.clone(), key.clone()).await);

        set_setting(&db.pool, settings::SettingKey::RequireUnlock, "on").await;
        let within_the_ttl = require_unlock(app.clone(), key.clone()).await;
        clock.advance(app.config().credential_ttl + Duration::from_secs(1));
        let past_the_ttl = require_unlock(app.clone(), key.clone()).await;

        assert!(!within_the_ttl, "younger settings are not read again");
        assert!(past_the_ttl, "the principal carries the new setting");
        assert!(app.settings().require_unlock, "and so does the app");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_requests_past_the_ttl_reload_settings_once() {
        let db = test_db().await;
        let (app, clock) = app_with_movable_clock(&db.pool).await;
        set_setting(&db.pool, settings::SettingKey::MaxKeysPerUser, "5").await;
        clock.advance(app.config().credential_ttl + Duration::from_secs(1));
        let loads_before = app.settings_loads.load(Ordering::Relaxed);
        let checks_before = app.settings_checks.load(Ordering::Relaxed);

        // A `JoinSet` runs the ten calls as concurrent tasks.
        let mut calls = tokio::task::JoinSet::new();
        for _ in 0..10 {
            let app = Arc::clone(&app);
            calls.spawn(async move { app.fresh_settings().await.max_keys_per_user });
        }
        let seen = tokio::time::timeout(Duration::from_secs(30), calls.join_all())
            .await
            .expect("ten settings checks within 30 s");

        assert_eq!(seen, [5; 10]);
        assert_eq!(
            app.settings_loads.load(Ordering::Relaxed) - loads_before,
            1,
            "one load for all ten"
        );
        assert_eq!(
            app.settings_checks.load(Ordering::Relaxed) - checks_before,
            1,
            "and one check: the rest found them fresh once they had the lock"
        );
    }

    /// Ten callers queued behind the check's lock with the database gone,
    /// after `mark` (nothing, for an expired TTL; a stale mark, as a poll,
    /// reconnect or `Settings` message makes). Returns how many checks the
    /// ten ran and what they got, then how many a later request ran.
    async fn queued_behind_a_failed_check(
        app: &Arc<App>,
        mark: impl FnOnce(&App),
    ) -> (usize, Vec<u32>, usize) {
        // The app's pool, closed: every check fails at once.
        app.pool().close().await;
        mark(app);
        let checks_before = app.settings_checks.load(Ordering::Relaxed);
        // Hold the check's lock so all ten queue behind it first.
        let held = app.settings_reload.lock().await;
        let mut calls = tokio::task::JoinSet::new();
        for _ in 0..10 {
            let app = Arc::clone(app);
            calls.spawn(async move { app.fresh_settings().await.max_keys_per_user });
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(held);
        let seen = tokio::time::timeout(Duration::from_secs(30), calls.join_all())
            .await
            .expect("ten settings checks within 30 s");
        let queued = app.settings_checks.load(Ordering::Relaxed) - checks_before;
        app.fresh_settings().await;
        let later = app.settings_checks.load(Ordering::Relaxed) - checks_before - queued;
        (queued, seen, later)
    }

    /// With the database gone, callers that queued behind one failing check
    /// take its answer (the loaded settings) instead of each running their
    /// own, one after another; a request after it still retries.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_requests_share_one_failed_check() {
        let db = test_db().await;
        let (app, clock) = app_with_movable_clock(&db.pool).await;
        clock.advance(app.config().credential_ttl + Duration::from_secs(1));

        let (queued, seen, later) = queued_behind_a_failed_check(&app, |_| {}).await;

        assert_eq!(seen, [20; 10], "the loaded settings serve");
        assert_eq!(queued, 1, "one failing check for all ten");
        assert_eq!(later, 1, "a later request checks again");
    }

    /// The same when a stale mark started the check: the mark its failure
    /// restores for the retry is not a newer one, so it is still shared.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_requests_share_one_failed_check_after_a_stale_mark() {
        let db = test_db().await;
        let (app, _clock) = app_with_movable_clock(&db.pool).await;

        let (queued, seen, later) =
            queued_behind_a_failed_check(&app, |app| app.mark_settings_stale()).await;

        assert_eq!(seen, [20; 10], "the loaded settings serve");
        assert_eq!(queued, 1, "one failing check for all ten");
        assert_eq!(later, 1, "a later request checks again");
    }

    /// A mark that lands while a check runs is newer than that check: a
    /// caller waiting behind it runs its own.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_mark_during_a_check_gets_a_check_of_its_own() {
        let db = test_db().await;
        let (app, _clock) = app_with_movable_clock(&db.pool).await;
        set_setting(&db.pool, settings::SettingKey::MaxKeysPerUser, "5").await;
        app.mark_settings_stale();
        // Stands in for a check that began before the mark: the lock held,
        // the round finished and the marks it saw recorded as of before.
        let held = app.settings_reload.lock().await;
        let waiter = {
            let app = Arc::clone(&app);
            tokio::spawn(async move { app.fresh_settings().await.max_keys_per_user })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        app.settings_rounds.fetch_add(1, Ordering::Release);
        drop(held);
        let seen = tokio::time::timeout(Duration::from_secs(30), waiter)
            .await
            .expect("the waiter within 30 s")
            .expect("the waiter's task");

        assert_eq!(seen, 5, "the waiter checked for its own mark");
    }

    /// A check that finds `admin_generation` unmoved loads nothing; one
    /// marked stale checks again at once, whatever the settings' age.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_check_loads_the_settings_only_when_admin_generation_moved() {
        let db = test_db().await;
        let (app, clock) = app_with_movable_clock(&db.pool).await;
        clock.advance(app.config().credential_ttl + Duration::from_secs(1));
        app.fresh_settings().await;
        let unmoved = app.settings_loads.load(Ordering::Relaxed);

        set_setting(&db.pool, settings::SettingKey::MaxKeysPerUser, "5").await;
        let fresh_and_unmarked = app.fresh_settings().await.max_keys_per_user;
        app.mark_settings_stale();
        let marked = app.fresh_settings().await.max_keys_per_user;

        assert_eq!(unmoved, 0, "an unmoved counter loads nothing");
        assert_eq!(fresh_and_unmarked, 20, "young settings are not checked");
        assert_eq!(marked, 5, "stale ones are");
    }

    /// A clock stepped back past the last check must not keep the settings
    /// forever: a check time in the future counts as expired.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_clock_stepped_back_expires_the_settings() {
        let db = test_db().await;
        let (app, clock) = app_with_movable_clock(&db.pool).await;
        set_setting(&db.pool, settings::SettingKey::MaxKeysPerUser, "5").await;
        *clock.0.lock().expect("clock lock") -= chrono::Duration::hours(1);

        assert_eq!(app.fresh_settings().await.max_keys_per_user, 5);
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

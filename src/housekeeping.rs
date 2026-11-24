//! Housekeeping as apalis jobs, run once per cluster (issue #693, folding in
//! #356; M18 spec Decisions 15 and 16), and once per store for the work on
//! catalog data (catalog stores plan Task 16, issue #976).
//!
//! The pruning and the cluster-wide counts used to run in every replica's
//! poll loop, so N replicas ran every delete N times and the work was
//! invisible. Now they are one apalis job type, [`Housekeeping`], on queue
//! [`QUEUE`]:
//!
//! 1. **Scheduling.** Every replica's ticker ([`tick_until`]) wakes once a
//!    minute and, for each [`Task`], pushes the jobs of the task's current
//!    slot ([`schedule`]). A slot is the start of the task's cadence window,
//!    as a Unix second. A task has a cluster job ([`Task::runs_on_cluster`]),
//!    keyed `"<task>:<slot>"`, and, for work on catalog data, one job per
//!    store ([`Task::runs_per_store`]), keyed `"<task>:<store>:<slot>"`. The
//!    stores are read from the `stores` table on every tick, so a store added
//!    after start-up gets its jobs from the next tick on, opened or not. A
//!    push first looks the key up and stops if the slot is scheduled (the
//!    common case, which so attempts no insert and logs nothing in
//!    Postgres); apalis-postgres's unique index on
//!    `(job_type, idempotency_key)` turns the rare insert that loses a race
//!    with another replica into a unique violation, which is ignored. Each
//!    slot is therefore scheduled once in the cluster, per store. A slot
//!    missed because every replica was down is not caught up; the next slot
//!    does the same work.
//! 2. **Running.** Every replica runs a housekeeping worker
//!    ([`spawn_worker`]) on the process's [`WorkerPool`]. apalis takes a job
//!    with `FOR UPDATE SKIP LOCKED`, so whichever worker takes it runs it
//!    alone. The worker polls every [`POLL_INTERVAL`] and opens no `LISTEN`
//!    connection: one of the pool's [`crate::mail::queue::WORKER_POOL_SIZE`]
//!    connections serves its fetches, acks and heartbeats. The tasks' own
//!    deletes and counts run on the request pool, as the poll loop's did, or
//!    on the store's pool.
//! 3. **Per-store jobs** ([`run_on_store`]): `prune_changesets` (all of its
//!    work), the store half of `prune_audit` ([`audit::prune_catalog`]) and
//!    the open changeset count of `cluster_stats`. Each job touches one
//!    store, so a store that is down fails its own jobs only. Every step is
//!    bounded ([`crate::stores::bounded`]): its statements run under
//!    `lock_timeout` and `statement_timeout`, and the step, opening the store
//!    included, under a `tokio` backstop. No step holds a connection of one
//!    database while it takes one of the other. A job whose store has no
//!    `stores` row any more does nothing and succeeds (it is not retried);
//!    a store the registry forgot ([`crate::stores::Stores::forget`], after
//!    its row changed) is opened again by its next job, from the new row.
//! 4. **Failures.** A failed task returns a plain error: apalis marks the job
//!    `Failed`, the handler moves its `run_at` back by the mail queue's
//!    backoff (30 s, doubling), and it is tried again, [`MAX_ATTEMPTS`] times
//!    in all. Every task is idempotent, so a retry, or a job re-run after its
//!    worker died, does no harm.
//! 5. **Gauges.** Each store's `cluster_stats` job writes its open changeset
//!    count to `cluster_store_stats` (migration `0043_cluster_store_stats.sql`);
//!    the cluster's job sums those rows, with the mail queue's depth, into the
//!    one-row `cluster_stats` table (migration `0024_cluster_stats.sql`). A
//!    store whose count fails keeps its last row, so the sum carries its last
//!    known count. Each replica's ticker reads the row once a minute into its
//!    gauges ([`refresh_gauges`]), so every replica reports the same values
//!    and a scrape never queries Postgres.

use std::sync::Arc;
use std::time::Duration;

use apalis::prelude::{BackendExt, BoxDynError, Data, TaskBuilder, TaskId, WorkerBuilder};
use apalis_postgres::queries::push_tasks;
use apalis_postgres::{Config as QueueConfig, PostgresStorage};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::error::OkfError;
use crate::mail::queue::{
    self as mail_queue, process_worker_name, reschedule_job, WorkerPool, WorkerTiming,
};
use crate::store::oauth::{
    clients as oauth_clients, codes as sign_in_codes, tokens as oauth_tokens,
};
use crate::store::{
    audit, catalogs, changesets, cleanups, email_tokens, keys, sign_in_events, user_sessions,
};
use crate::stores::bounded::{account_tx, backstop, store_tx, Limits, LIMITS};
use crate::stores::{AccountDb, CatalogDb, StoreName};
use crate::telemetry::metrics::{MailStatus, Outcome};
use crate::telemetry::subscriber::{split_event, LOCAL_ONLY};

/// The apalis queue (`apalis.jobs.job_type`). Set explicitly: apalis's
/// default is the Rust type's path, which a rename would orphan.
pub const QUEUE: &str = "housekeeping";

/// `max_attempts` for every housekeeping job: a failure is retried twice,
/// then the next slot does the work again.
pub const MAX_ATTEMPTS: usize = 3;

/// How often each replica's ticker schedules the current slots and reads
/// the `cluster_stats` row: the shortest cadence.
pub const TICK_INTERVAL: Duration = Duration::from_secs(60);

/// How often an idle housekeeping worker looks for due jobs. It has no
/// `LISTEN`, so this is also how late a newly scheduled job may start.
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// The worker-name prefix: `okf-housekeeping-<host>-<pid>`.
const WORKER_PREFIX: &str = "okf-housekeeping";

/// The name of apalis-postgres's unique index on
/// `(job_type, idempotency_key)` (its migration `20260508093314`).
const IDEMPOTENCY_INDEX: &str = "idx_jobs_idempotency_key";

/// Postgres's SQLSTATE for a unique violation.
const UNIQUE_VIOLATION: &str = "23505";

/// How long an empty open changeset may go unchanged before it is
/// discarded (design "Session lifecycle").
const EMPTY_CHANGESET_HOURS: i64 = 24;

/// How old a half-created catalog must be before the `provision_catalogs`
/// task looks at it: an efficiency filter that keeps it off creates still
/// running. Safety does not depend on it (decision P9).
const UNPROVISIONED_GRACE_SECONDS: i64 = 60;

/// How long expired tokens and unredeemed codes are kept (for diagnosis).
const OAUTH_RETENTION_DAYS: i64 = 7;

/// How long a registered client that no grant names is kept.
const UNBOUND_CLIENT_HOURS: i64 = 24;

/// How long sign-in events are kept (spec "Logging and audit").
const SIGN_IN_EVENT_RETENTION_DAYS: i64 = 90;

/// How far past the retention period the audit prune's cut-off lies, so a
/// small skew between this clock and the database's (whose clock the
/// append-only trigger uses) never makes it refuse the prune.
const AUDIT_PRUNE_MARGIN_HOURS: i64 = 1;

/// How long finished housekeeping jobs are kept (a day of runs to inspect).
const DONE_JOB_RETENTION_HOURS: i64 = 24;

/// How long housekeeping jobs that gave up are kept, as for mail.
const FAILED_JOB_RETENTION_DAYS: i64 = 30;

/// One housekeeping task (spec Decision 15).
// `serde(rename_all)`: stored and keyed as `prune_oauth`, not `PruneOauth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Task {
    /// Discards empty changesets unchanged for a day, one job per store;
    /// every 5 minutes.
    PruneChangesets,
    /// Revokes idle OAuth grants, then deletes expired tokens, codes,
    /// unbound clients, old sign-in events, expired emailed links, console
    /// sessions, old mail and housekeeping jobs and forwarded feedback;
    /// hourly.
    PruneOauth,
    /// Deletes audit entries past `audit_retention_days`: the account
    /// database's in the cluster job, each store's catalog events in a job
    /// of its own; daily.
    PruneAudit,
    /// Deletes the `apalis.workers` rows of stopped workers; hourly.
    PruneWorkers,
    /// Counts each store's open changesets (a job per store, into
    /// `cluster_store_stats`), and sums them with the mail queue into
    /// `cluster_stats` (the cluster job); every minute.
    ClusterStats,
    /// Finishes or removes catalogs whose create stopped half-way (plan
    /// Task 10b, decision P9); every minute.
    ProvisionCatalogs,
    /// Settles the cleanup owed on every catalog store after a deletion on
    /// the account database: a deleted catalog's data, a deleted key's open
    /// changesets (plan P13, issue #972); every minute.
    CleanupStores,
}

impl Task {
    /// Every task, in the order the ticker schedules them.
    pub const ALL: [Self; 7] = [
        Self::PruneChangesets,
        Self::PruneOauth,
        Self::PruneAudit,
        Self::PruneWorkers,
        Self::ClusterStats,
        Self::ProvisionCatalogs,
        Self::CleanupStores,
    ];

    /// The task's name: its idempotency-key prefix, its metric label and
    /// its serialized form.
    ///
    /// # Returns
    /// A fixed snake_case word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PruneChangesets => "prune_changesets",
            Self::PruneOauth => "prune_oauth",
            Self::PruneAudit => "prune_audit",
            Self::PruneWorkers => "prune_workers",
            Self::ClusterStats => "cluster_stats",
            Self::ProvisionCatalogs => "provision_catalogs",
            Self::CleanupStores => "cleanup_stores",
        }
    }

    /// Whether the task has a cluster job each slot, keyed `"<task>:<slot>"`.
    ///
    /// # Returns
    /// `false` only for a task whose work is all on the stores.
    pub const fn runs_on_cluster(self) -> bool {
        match self {
            Self::PruneChangesets => false,
            Self::PruneOauth
            | Self::PruneAudit
            | Self::PruneWorkers
            | Self::ClusterStats
            | Self::ProvisionCatalogs
            | Self::CleanupStores => true,
        }
    }

    /// Whether the task has a job for each store each slot, keyed
    /// `"<task>:<store>:<slot>"`: the tasks that work on catalog data.
    /// `cleanup_stores` is not one: its one job settles every store's rows,
    /// each store contained (issue #972).
    ///
    /// # Returns
    /// `true` for `prune_changesets`, `prune_audit` and `cluster_stats`.
    pub const fn runs_per_store(self) -> bool {
        match self {
            Self::PruneChangesets | Self::PruneAudit | Self::ClusterStats => true,
            Self::PruneOauth
            | Self::PruneWorkers
            | Self::ProvisionCatalogs
            | Self::CleanupStores => false,
        }
    }

    /// How often the task runs in the cluster.
    ///
    /// # Returns
    /// 5 minutes, an hour, a day, an hour, or a minute.
    pub const fn cadence(self) -> Duration {
        match self {
            Self::PruneChangesets => Duration::from_secs(5 * 60),
            Self::PruneOauth | Self::PruneWorkers => Duration::from_secs(60 * 60),
            Self::PruneAudit => Duration::from_secs(24 * 60 * 60),
            Self::ClusterStats | Self::ProvisionCatalogs | Self::CleanupStores => {
                Duration::from_secs(60)
            }
        }
    }

    /// The start of the cadence window containing `now`, in Unix seconds:
    /// every replica computes the same slot within one window.
    ///
    /// # Arguments
    /// * `now` - the server clock.
    ///
    /// # Returns
    /// `now` rounded down to a multiple of the cadence (UTC, so the daily
    /// slot starts at midnight UTC).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn slot_at(self, now: DateTime<Utc>) -> i64 {
        // Every cadence is a few seconds to a day, so the cast is exact.
        let cadence = self.cadence().as_secs() as i64;
        // `div_euclid` rounds towards minus infinity, so a time before 1970
        // still lands at its window's start (Python's `//`).
        now.timestamp().div_euclid(cadence) * cadence
    }
}

/// The job: which task, for which slot, and on which store. The slot and
/// the store are in the payload as well as the key, so a job's span and log
/// lines can name them. The payload carries the store's name only, never
/// catalog content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Housekeeping {
    /// The task to run.
    pub task: Task,
    /// The slot it was scheduled for ([`Task::slot_at`]).
    pub slot: i64,
    /// The store a per-store job runs on; `None` for the cluster's job.
    // `default`: a payload written before stores (no `store` key) reads as
    // a cluster job; `skip_serializing_if`: a cluster job's payload is as it
    // was. `StoreName` deserializes through `parse`, so a bad name is a
    // payload error, not a store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<StoreName>,
}

impl Housekeeping {
    /// The cluster job of `task` for `slot`.
    ///
    /// # Arguments
    /// * `task` - the task.
    /// * `slot` - its slot ([`Task::slot_at`]).
    ///
    /// # Returns
    /// The job.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "builds a value")
    )]
    pub const fn cluster(task: Task, slot: i64) -> Self {
        Self {
            task,
            slot,
            store: None,
        }
    }

    /// The job of `task` on `store` for `slot`.
    ///
    /// # Arguments
    /// * `task` - the task.
    /// * `store` - the store.
    /// * `slot` - its slot ([`Task::slot_at`]).
    ///
    /// # Returns
    /// The job.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "builds a value")
    )]
    pub const fn on_store(task: Task, store: StoreName, slot: i64) -> Self {
        Self {
            task,
            slot,
            store: Some(store),
        }
    }

    /// The job's idempotency key: `"<task>:<slot>"` for a cluster job,
    /// `"<task>:<store>:<slot>"` for a store's. A store name has no `:`, so
    /// the two never collide.
    ///
    /// # Returns
    /// The key.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "formats a value")
    )]
    pub fn key(&self) -> String {
        match &self.store {
            None => format!("{}:{}", self.task.as_str(), self.slot),
            Some(store) => format!("{}:{store}:{}", self.task.as_str(), self.slot),
        }
    }
}

/// The error for a queue operation apalis refused (its own error type).
#[tracing::instrument(level = "trace", skip_all)]
fn queue_error(error: impl std::fmt::Display) -> OkfError {
    OkfError::Io {
        path: "<housekeeping queue>".to_string(),
        source: std::io::Error::other(error.to_string()),
    }
}

/// Pushes `job` unless one with its key ([`Housekeeping::key`]) exists. It
/// first looks the key up (through apalis's unique index), so the common
/// case, a slot another replica or an earlier tick already scheduled,
/// attempts no insert and leaves no error in the Postgres log. Only two
/// replicas racing for a new slot both get past the lookup: then the unique
/// index makes the second push a unique violation, which is ignored. The
/// lookup and the push run in a transaction of their own, bounded at the
/// database by [`LIMITS`] (a lock held on the queue fails the push instead of
/// holding the tick), so the violation aborts nothing else.
///
/// # Arguments
/// * `pool` - the account database.
/// * `job` - the job.
///
/// # Returns
/// `true` if this call created the job, `false` if the slot already had one.
///
/// # Errors
/// [`OkfError::Database`] or [`OkfError::Io`] if the insert fails for any
/// other reason; `Busy` past the backstop.
#[tracing::instrument(level = "debug", skip_all, fields(okf.housekeeping.task = job.task.as_str()))]
pub async fn schedule(pool: &PgPool, job: &Housekeeping) -> Result<bool, OkfError> {
    backstop(LIMITS, push_once(pool, job)).await
}

/// [`schedule`]'s body, without the backstop.
///
/// # Errors
/// As [`schedule`].
#[tracing::instrument(level = "trace", skip_all)]
async fn push_once(pool: &PgPool, job: &Housekeeping) -> Result<bool, OkfError> {
    let key = job.key();
    let mut tx = account_tx(&AccountDb::new(pool.clone()), LIMITS.wait).await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM apalis.jobs WHERE job_type = $1 AND idempotency_key = $2)",
    )
    .bind(QUEUE)
    .bind(&key)
    .fetch_one(&mut *tx)
    .await?;
    if exists {
        return Ok(false);
    }
    let payload = serde_json::to_vec(job).map_err(queue_error)?;
    let task = TaskBuilder::new(payload)
        .max_attempts(MAX_ATTEMPTS)
        .idempotency_key(key)
        .build();
    // A failed push leaves `tx` aborted; dropping it rolls it back.
    match push_tasks(&mut *tx, QUEUE, vec![task]).await {
        Ok(()) => {
            tx.commit().await?;
            Ok(true)
        }
        Err(apalis_postgres::Error::Database(sqlx::Error::Database(error)))
            if error.code().as_deref() == Some(UNIQUE_VIOLATION)
                && error.constraint() == Some(IDEMPOTENCY_INDEX) =>
        {
            Ok(false)
        }
        Err(apalis_postgres::Error::Database(error)) => Err(OkfError::from(error)),
        Err(error) => Err(queue_error(error)),
    }
}

/// Every [`TICK_INTERVAL`] until `shutdown` (the first at once): schedules
/// the current slot of every task, then reads the `cluster_stats` row into
/// this replica's gauges. A failure is logged and the next tick tries again.
/// A tick in progress is dropped when `shutdown` is cancelled.
///
/// # Arguments
/// * `app` - the shared state (its pool, clock and metrics).
/// * `shutdown` - cancelled when the server stops.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "long-lived loop: its span would never close and would stay open as the parent of every later span"
    )
)]
pub async fn tick_until(app: Arc<App>, shutdown: CancellationToken) {
    let mut ticks = tokio::time::interval(TICK_INTERVAL);
    // After a slow tick, wait a full interval instead of catching up.
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        // `select!` waits for whichever branch is ready first.
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = ticks.tick() => {}
        }
        // The tick is raced against shutdown too, so a query blocked on a
        // lock cannot hold the process past `worker_shutdown_grace`. Dropping
        // it midway is safe: scheduling is idempotent per slot (the next
        // replica or start pushes it again) and the gauge refresh only reads.
        if shutdown.run_until_cancelled(tick(&app)).await.is_none() {
            return;
        }
    }
}

/// One tick of [`tick_until`]: each its own trace (`parent: None`).
#[tracing::instrument(level = "debug", name = "okf.housekeeping.tick", parent = None, skip_all)]
async fn tick(app: &App) {
    schedule_slots(app, app.clock().now()).await;
    refresh_gauges(app).await;
}

/// Pushes every job of the slots containing `now`: each task's cluster job,
/// and its job on every store in the `stores` table (read now, so a store
/// added since the last tick is included). A failure is logged and the next
/// tick tries again; it stops neither another task nor another store. If
/// the stores cannot be read, this tick pushes cluster jobs only.
///
/// # Arguments
/// * `app` - the shared state (its pools and registry).
/// * `now` - the time whose slots are pushed.
#[tracing::instrument(level = "debug", skip_all)]
async fn schedule_slots(app: &App, now: DateTime<Utc>) {
    let stores = match app.stores().all().await {
        Ok(stores) => stores,
        Err(error) => {
            split_event!(
                warn,
                { kind = error.kind(), },
                { %error, },
                "reading the stores failed; their housekeeping jobs wait for the next tick"
            );
            Vec::new()
        }
    };
    let pool = app.accounts().executor();
    for task in Task::ALL {
        let slot = task.slot_at(now);
        let cluster = task
            .runs_on_cluster()
            .then(|| Housekeeping::cluster(task, slot));
        let per_store = stores
            .iter()
            .filter(|_| task.runs_per_store())
            .map(|store| Housekeeping::on_store(task, store.clone(), slot));
        // `chain`: the cluster job (if any) first, then one per store.
        for job in cluster.into_iter().chain(per_store) {
            if let Err(error) = schedule(pool, &job).await {
                split_event!(
                    warn,
                    { task = task.as_str(), store = job.store.as_ref().map_or("", StoreName::as_str), kind = error.kind(), },
                    { %error, },
                    "scheduling a housekeeping job failed; retrying at the next tick"
                );
            }
        }
    }
}

/// The counts the `cluster_stats` task stores, as one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterStats {
    /// Open changesets in every catalog.
    pub open_changesets: i64,
    /// Mail jobs not yet sent, by status; a missing status has none.
    pub mail_queue: Vec<(MailStatus, i64)>,
    /// When the job computed them (server clock).
    pub computed_at: DateTime<Utc>,
}

/// Reads the `cluster_stats` row.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The stored counts, or `None` before the first `cluster_stats` job ran.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all)]
pub async fn read_stats(pool: &PgPool) -> Result<Option<ClusterStats>, OkfError> {
    let row: Option<(i64, serde_json::Value, DateTime<Utc>)> = sqlx::query_as(
        "SELECT open_changesets, mail_queue, computed_at FROM cluster_stats WHERE id",
    )
    .fetch_optional(pool)
    .await?;
    // `map` over the `Option`: `None` stays `None`.
    Ok(row.map(|(open_changesets, mail_queue, computed_at)| {
        // A status the row lacks (or holds as a non-number) counts as none.
        let mail_queue = MailStatus::ALL
            .into_iter()
            .filter_map(|status| {
                let jobs = mail_queue.get(status.as_str())?.as_i64()?;
                Some((status, jobs))
            })
            .collect();
        ClusterStats {
            open_changesets,
            mail_queue,
            computed_at,
        }
    }))
}

/// Reads the `cluster_stats` row into this replica's `okf.changesets.open`
/// and `okf.mail.queue.depth` gauges. Without a row (no `cluster_stats` job
/// has run yet) or on a failure the gauges keep their last values; a failure
/// is logged.
///
/// # Arguments
/// * `app` - the shared state (its pool and metrics).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn refresh_gauges(app: &App) {
    match read_stats(app.accounts().executor()).await {
        Ok(Some(stats)) => {
            app.metrics().set_open_changesets(stats.open_changesets);
            app.metrics().set_mail_queue_depth(&stats.mail_queue);
        }
        Ok(None) => {}
        Err(error) => {
            split_event!(
                warn,
                { kind = error.kind(), },
                { %error, },
                "reading the cluster stats failed; keeping the last values"
            );
        }
    }
}

/// Runs one task's cluster job now, in this process. The worker calls it
/// for each cluster job; tests call it directly. A task whose work is all on
/// the stores (`prune_changesets`) does nothing here: a cluster job of it
/// can only come from a release before stores, and that slot's store jobs
/// do its work.
///
/// # Arguments
/// * `task` - the task.
/// * `app` - the shared state.
///
/// # Errors
/// The first failure among the task's steps ([`OkfError::Database`] mostly).
/// A task with several steps runs them all and logs each failure.
#[tracing::instrument(level = "debug", skip_all, fields(okf.housekeeping.task = task.as_str()))]
pub async fn run(task: Task, app: &App) -> Result<(), OkfError> {
    match task {
        Task::PruneChangesets => Ok(()),
        Task::PruneOauth => prune_oauth(app).await,
        Task::PruneAudit => prune_audit(app).await,
        Task::PruneWorkers => {
            let mut steps = Steps::default();
            steps.note(
                "rows of stopped workers",
                mail_queue::prune_workers(app.accounts().executor(), app.clock().now()).await,
            );
            steps.finish()
        }
        Task::ClusterStats => cluster_stats(app).await,
        Task::ProvisionCatalogs => provision_catalogs(app).await,
        Task::CleanupStores => cleanup_stores(app).await,
    }
}

/// Runs one task's job on `store` now, in this process, under [`LIMITS`]:
/// the worker calls it for each store job; tests call it directly. Only
/// this store is touched, so its failure is its job's alone.
///
/// # Arguments
/// * `task` - the task.
/// * `store` - the store.
/// * `app` - the shared state.
///
/// # Errors
/// The step's failure: [`OkfError::StoreUnavailable`] for a store that cannot
/// be reached, [`OkfError::Database`] (a lock or statement past the bound
/// included), or `Busy` past the backstop. A store with no `stores` row is
/// not an error: the job has nothing to do.
#[tracing::instrument(level = "debug", skip_all, fields(okf.housekeeping.task = task.as_str(), okf.store = store.as_str()))]
pub async fn run_on_store(task: Task, store: &StoreName, app: &App) -> Result<(), OkfError> {
    run_on_store_bounded(task, store, app, LIMITS).await
}

/// [`run_on_store`] under `limits`.
///
/// # Errors
/// As [`run_on_store`].
#[tracing::instrument(level = "debug", skip_all)]
async fn run_on_store_bounded(
    task: Task,
    store: &StoreName,
    app: &App,
    limits: Limits,
) -> Result<(), OkfError> {
    if !task.runs_per_store() {
        // Only a hand-made job can be one: nothing schedules it.
        tracing::warn!(
            task = task.as_str(),
            "a store job of a task that has none; ignored"
        );
        return Ok(());
    }
    let Some(db) = open_store(app, store, limits).await? else {
        return Ok(());
    };
    match task {
        Task::PruneChangesets => prune_store_changesets(app, &db, limits).await,
        Task::PruneAudit => prune_store_audit(app, &db, limits).await,
        Task::ClusterStats => count_store_changesets(app, &db, limits).await,
        // Returned above (`runs_per_store` is false for these).
        Task::PruneOauth | Task::PruneWorkers | Task::ProvisionCatalogs | Task::CleanupStores => {
            Ok(())
        }
    }
}

/// Opens `store` for a store job, under the backstop (the registry's row
/// read is bounded at the database too: issue #1006).
///
/// # Returns
/// The store, or `None` if it has no `stores` row (logged; the job has
/// nothing to do and is not retried).
///
/// # Errors
/// [`OkfError::StoreUnavailable`], `Busy`, or [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all)]
async fn open_store(
    app: &App,
    store: &StoreName,
    limits: Limits,
) -> Result<Option<CatalogDb>, OkfError> {
    match backstop(limits, app.stores().get(store)).await {
        Ok(db) => Ok(Some(db)),
        Err(OkfError::NotFound { .. }) => {
            tracing::info!(
                store = store.as_str(),
                "a housekeeping job's store has no row; nothing to do"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// The outcome of a task's steps: each step's result is logged as it is
/// noted, and the first failure is kept for the job's result.
#[derive(Debug, Default)]
struct Steps {
    first_failure: Option<OkfError>,
}

impl Steps {
    /// Logs one step's outcome (silent for nothing deleted, `info` for some,
    /// `error` for a failure) and keeps a first failure.
    ///
    /// # Arguments
    /// * `what` - the rows, for the log line.
    /// * `result` - how many were deleted, or the failure.
    #[tracing::instrument(level = "trace", skip_all)]
    fn note(&mut self, what: &'static str, result: Result<u64, OkfError>) {
        match result {
            Ok(0) => {}
            Ok(pruned) => tracing::info!(pruned, "deleted {what}"),
            Err(error) => {
                // Not `split_event!`: its message must be a literal.
                tracing::error!(kind = error.kind(), "deleting {what} failed");
                tracing::error!(target: LOCAL_ONLY, %error, "deleting {what} failed (detail)");
                self.keep(error);
            }
        }
    }

    /// Keeps `error` as the task's failure unless an earlier step failed.
    ///
    /// # Arguments
    /// * `error` - a step's failure, already logged.
    #[tracing::instrument(level = "trace", skip_all)]
    fn keep(&mut self, error: OkfError) {
        // `get_or_insert`: keeps an earlier failure if there is one.
        self.first_failure.get_or_insert(error);
    }

    /// The task's result: `Ok` unless a step failed.
    ///
    /// # Errors
    /// The first failure noted.
    #[tracing::instrument(level = "trace", skip_all)]
    fn finish(self) -> Result<(), OkfError> {
        // `Option<E>` to `Result<(), E>`: `None` is `Ok(())`.
        self.first_failure.map_or(Ok(()), Err)
    }
}

/// Discards the empty changesets on `db` unchanged for
/// [`EMPTY_CHANGESET_HOURS`], in one bounded store transaction.
///
/// # Errors
/// [`OkfError::Database`], [`OkfError::StoreUnavailable`] or `Busy`.
#[tracing::instrument(level = "debug", skip_all)]
async fn prune_store_changesets(app: &App, db: &CatalogDb, limits: Limits) -> Result<(), OkfError> {
    let now = app.clock().now();
    let older_than = now - TimeDelta::hours(EMPTY_CHANGESET_HOURS);
    let pruned = backstop(limits, async {
        let mut tx = store_tx(db, limits.wait).await?;
        let pruned = changesets::prune_empty(&mut *tx, older_than, now).await?;
        tx.commit().await.map_err(|error| db.classify(error))?;
        Ok(pruned)
    })
    .await;
    let mut steps = Steps::default();
    steps.note("empty changesets a day old", pruned);
    steps.finish()
}

/// Finishes or removes the catalogs whose create stopped half-way
/// ([`catalogs::recover`], decision P9). It looks at rows older than
/// [`UNPROVISIONED_GRACE_SECONDS`]; that age only keeps it off creates still
/// running, since the catalog's lock decides the outcome. A failure on one
/// catalog (its store unreachable, say) is logged and does not stop the
/// others; the first is the task's result, so the job is retried.
///
/// # Errors
/// The first failure; every catalog is tried regardless.
#[tracing::instrument(level = "debug", skip_all)]
async fn provision_catalogs(app: &App) -> Result<(), OkfError> {
    let now = app.clock().now();
    let older_than = now - TimeDelta::seconds(UNPROVISIONED_GRACE_SECONDS);
    let half_made = catalogs::pending(app.accounts().executor(), older_than).await?;
    let mut steps = Steps::default();
    let mut finished = 0_u64;
    for catalog in &half_made {
        // The store is read from the row, so a catalog is recovered where
        // it was placed.
        let outcome = async {
            let store = StoreName::parse(&catalog.store)?;
            let db = app.store_db(&store).await?;
            catalogs::recover(&db, catalog, now).await
        }
        .await;
        match outcome {
            Ok(catalogs::Recovery::Finished) => finished += 1,
            Ok(catalogs::Recovery::Removed) => {
                tracing::warn!(
                    catalog_id = catalog.id,
                    "removed a catalog whose create failed"
                );
            }
            Ok(catalogs::Recovery::Settled) => {}
            Err(error) => {
                split_event!(
                    error,
                    { kind = error.kind(), catalog_id = catalog.id, },
                    { %error, },
                    "recovering a half-created catalog failed"
                );
                steps.keep(error);
            }
        }
    }
    if finished > 0 {
        tracing::info!(finished, "finished half-created catalogs");
    }
    steps.finish()
}

/// Settles the cleanup owed on the stores ([`cleanups::run`]). A store that
/// cannot be reached, or a row that fails, is logged and counted in the
/// row's `attempts` and retried by the next run; it neither stops the other
/// stores nor fails the job, so the minute's slot is not re-run for it.
///
/// # Errors
/// [`OkfError::Database`] reading the account database's list of cleanups.
#[tracing::instrument(level = "debug", skip_all)]
async fn cleanup_stores(app: &App) -> Result<(), OkfError> {
    let report = cleanups::run(app.stores(), app.clock().now()).await?;
    if report.done > 0 {
        tracing::info!(done = report.done, "settled store cleanups");
    }
    Ok(())
}

/// Revokes idle OAuth grants ([`revoke_idle_grants`]), then deletes expired
/// OAuth tokens and unredeemed codes kept [`OAUTH_RETENTION_DAYS`] past their
/// expiry, registered clients no grant names after [`UNBOUND_CLIENT_HOURS`],
/// sign-in events older than [`SIGN_IN_EVENT_RETENTION_DAYS`], emailed links
/// [`OAUTH_RETENTION_DAYS`] past their expiry, console sessions past their
/// absolute limit, mail jobs (sent ones after 7 days and failed ones after
/// 30), finished housekeeping jobs ([`prune_jobs`]) and forwarded feedback
/// after `[feedback] retention` (default 30 days). Runs whether or not OAuth
/// is on, so rows left from when it was are cleared too.
///
/// # Errors
/// The first step's failure; every step runs regardless.
#[tracing::instrument(level = "debug", skip_all)]
async fn prune_oauth(app: &App) -> Result<(), OkfError> {
    let pool = app.accounts().executor();
    let now = app.clock().now();
    let mut steps = Steps::default();
    // Before the token prune, which may delete the tokens that date a
    // grant's last activity (it then falls back to the grant's creation,
    // which is older still).
    if let Err(error) = revoke_idle_grants(app, now).await {
        split_event!(
            error,
            { kind = error.kind(), },
            { %error, },
            "revoking idle OAuth grants failed"
        );
        steps.keep(error);
    }
    let retained = now - TimeDelta::days(OAUTH_RETENTION_DAYS);
    steps.note(
        "expired OAuth tokens",
        oauth_tokens::prune(pool, retained).await,
    );
    steps.note(
        "expired sign-in codes",
        sign_in_codes::prune(pool, retained).await,
    );
    steps.note(
        "unused registered OAuth clients",
        oauth_clients::prune_unbound_dcr(pool, now - TimeDelta::hours(UNBOUND_CLIENT_HOURS)).await,
    );
    steps.note(
        "old sign-in events",
        sign_in_events::prune(pool, now - TimeDelta::days(SIGN_IN_EVENT_RETENTION_DAYS)).await,
    );
    steps.note(
        "expired emailed links",
        email_tokens::prune(pool, retained).await,
    );
    steps.note(
        "expired console sessions",
        user_sessions::prune(pool, now).await,
    );
    steps.note("old mail jobs", mail_queue::prune(pool, now).await);
    steps.note("old housekeeping jobs", prune_jobs(pool, now).await);
    // Without `[feedback]` too, so rows left from when it was set go after
    // the default retention.
    let retention = app
        .config()
        .feedback
        .as_ref()
        .map_or(crate::config::DEFAULT_FEEDBACK_RETENTION, |feedback| {
            feedback.retention
        });
    // Validated to at most ten years, so the conversion succeeds.
    if let Ok(retention) = TimeDelta::from_std(retention) {
        steps.note(
            "forwarded feedback past retention",
            crate::mail::feedback::prune(pool, now, retention).await,
        );
    }
    steps.finish()
}

/// Revokes the OAuth grants idle for `oauth.idle_grant_revoke_after` that
/// hold no live token ([`keys::revoke_idle_grants`]); several replicas
/// running it at once would revoke and audit each grant once, and as a
/// housekeeping step it runs once per cluster per hour. Every replica's
/// credential cache drops the revoked grants on their change messages.
///
/// # Arguments
/// * `app` - the shared state.
/// * `now` - the job's time (server clock).
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all)]
async fn revoke_idle_grants(app: &App, now: DateTime<Utc>) -> Result<(), OkfError> {
    // The config check caps the period at 10 years, so both steps succeed;
    // `and_then` chains the two `Option`s, and `None` skips the step
    // instead of panicking.
    let Some(idle_before) = TimeDelta::from_std(app.config().oauth.idle_grant_revoke_after)
        .ok()
        .and_then(|period| now.checked_sub_signed(period))
    else {
        return Ok(());
    };
    let revoked = keys::revoke_idle_grants(app.accounts().executor(), idle_before, now).await?;
    if !revoked.is_empty() {
        tracing::info!(
            revoked = revoked.len(),
            "revoked idle OAuth grants that held no live token"
        );
    }
    Ok(())
}

/// The audit retention in days, and the cut-off it gives at `now`: the
/// `audit_retention_days` setting plus [`AUDIT_PRUNE_MARGIN_HOURS`] before
/// `now`.
///
/// # Returns
/// `(days, before)`.
#[tracing::instrument(level = "trace", skip_all)]
async fn audit_cut_off(app: &App, now: DateTime<Utc>) -> (i64, DateTime<Utc>) {
    // Outside any request, so it checks the settings itself (spec Decision
    // 9): the retention never runs on settings older than `credential_ttl`.
    let days = i64::from(app.fresh_settings().await.audit_retention_days);
    let before = now - TimeDelta::days(days) - TimeDelta::hours(AUDIT_PRUNE_MARGIN_HOURS);
    (days, before)
}

/// The account half of `prune_audit`: deletes the account database's audit
/// entries older than the `audit_retention_days` setting plus
/// [`AUDIT_PRUNE_MARGIN_HOURS`], and the provisioner budget counters of past
/// hours. Each store's catalog events are pruned by its own job
/// ([`prune_store_audit`]). Nothing else deletes entries.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all)]
async fn prune_audit(app: &App) -> Result<(), OkfError> {
    let now = app.clock().now();
    let (_, before) = audit_cut_off(app, now).await;
    let mut steps = Steps::default();
    steps.note(
        "audit entries past retention",
        audit::prune(app.accounts().executor(), before).await,
    );
    steps.note(
        "provisioner budget counters of past hours",
        audit::prune_budget_counters(app.accounts().executor(), now).await,
    );
    steps.finish()
}

/// The store half of `prune_audit`: deletes `db`'s catalog events past the
/// same cut-off as the account's ([`audit::prune_catalog`], which hands the
/// retention to the store's trigger in `okf.audit_retention_days`).
///
/// # Errors
/// [`OkfError::Database`], [`OkfError::StoreUnavailable`] or `Busy`.
#[tracing::instrument(level = "debug", skip_all)]
async fn prune_store_audit(app: &App, db: &CatalogDb, limits: Limits) -> Result<(), OkfError> {
    let (days, before) = audit_cut_off(app, app.clock().now()).await;
    let pruned = backstop(limits, audit::prune_catalog(db, before, days, limits.wait)).await;
    let mut steps = Steps::default();
    steps.note("catalog audit entries past retention", pruned);
    steps.finish()
}

/// Deletes housekeeping jobs finished over [`DONE_JOB_RETENTION_HOURS`]
/// before `now`, and ones that gave up over [`FAILED_JOB_RETENTION_DAYS`].
/// A deleted job's slot is long past, so its idempotency key is never
/// pushed again.
///
/// # Arguments
/// * `pool` - the database.
/// * `now` - the server clock.
///
/// # Returns
/// How many were deleted.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all)]
async fn prune_jobs(pool: &PgPool, now: DateTime<Utc>) -> Result<u64, OkfError> {
    Ok(sqlx::query(
        "DELETE FROM apalis.jobs WHERE job_type = $1 AND ( \
             (status = 'Done' AND done_at < $2) \
          OR ((status = 'Killed' OR (status = 'Failed' AND attempts >= max_attempts)) \
              AND done_at < $3))",
    )
    .bind(QUEUE)
    .bind(now - TimeDelta::hours(DONE_JOB_RETENTION_HOURS))
    .bind(now - TimeDelta::days(FAILED_JOB_RETENTION_DAYS))
    .execute(pool)
    .await?
    .rows_affected())
}

/// Sums every store's open changeset count (`cluster_store_stats`, written
/// by [`count_store_changesets`]) and counts the mail queue, and writes both
/// to the `cluster_stats` row (spec Decision 16). The sum is taken in the
/// statement that writes it, so it holds each store's latest count.
///
/// # Errors
/// [`OkfError::Database`] for a failed count or write; `Busy` past the
/// bound.
#[tracing::instrument(level = "debug", skip_all)]
async fn cluster_stats(app: &App) -> Result<(), OkfError> {
    let pool = app.accounts().executor();
    let depth = mail_queue::depth(pool).await?;
    // `{"pending": 2, ...}`: one key per status, zero included.
    let mail_queue: serde_json::Map<String, serde_json::Value> = MailStatus::ALL
        .into_iter()
        .map(|status| {
            let jobs: i64 = depth
                .iter()
                .filter(|(counted, _)| *counted == status)
                .map(|(_, jobs)| jobs)
                .sum();
            (status.as_str().to_string(), serde_json::Value::from(jobs))
        })
        .collect();
    let now = app.clock().now();
    backstop(LIMITS, async {
        let mut tx = account_tx(app.accounts(), LIMITS.wait).await?;
        // `sum` of bigints is a numeric: cast back. No rows sum to 0.
        sqlx::query(
            "INSERT INTO cluster_stats (id, open_changesets, mail_queue, computed_at) \
             SELECT true, COALESCE(sum(open_changesets), 0)::bigint, $1, $2 \
             FROM cluster_store_stats \
             ON CONFLICT (id) DO UPDATE SET open_changesets = EXCLUDED.open_changesets, \
                 mail_queue = EXCLUDED.mail_queue, computed_at = EXCLUDED.computed_at",
        )
        .bind(serde_json::Value::Object(mail_queue))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    })
    .await
}

/// The store half of `cluster_stats`: counts `db`'s open changesets in a
/// bounded store transaction, gives that connection back, then writes the
/// count to the store's `cluster_store_stats` row in a bounded account
/// transaction. The two never overlap, so a pool of one connection on
/// either side is enough.
///
/// # Errors
/// [`OkfError::Database`], [`OkfError::StoreUnavailable`] or `Busy`; a
/// failed count leaves the store's last row as it was.
#[tracing::instrument(level = "debug", skip_all)]
async fn count_store_changesets(app: &App, db: &CatalogDb, limits: Limits) -> Result<(), OkfError> {
    let open = backstop(limits, async {
        let mut tx = store_tx(db, limits.wait).await?;
        let open = changesets::open_total(&mut *tx).await?;
        tx.commit().await.map_err(|error| db.classify(error))?;
        Ok(open)
    })
    .await?;
    let now = app.clock().now();
    backstop(limits, async {
        let mut tx = account_tx(app.accounts(), limits.wait).await?;
        sqlx::query(
            "INSERT INTO cluster_store_stats (store, open_changesets, computed_at) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (store) DO UPDATE SET open_changesets = EXCLUDED.open_changesets, \
                 computed_at = EXCLUDED.computed_at",
        )
        .bind(db.store().as_str())
        .bind(open)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    })
    .await
}

/// What the job handler needs, given to apalis as worker data (cloned per
/// job: both fields are cheap handles).
#[derive(Clone)]
pub(crate) struct HousekeepingContext {
    /// The shared state the tasks run against (its request pool).
    pub app: Arc<App>,
    /// The process's [`WorkerPool`], for the handler's reschedules.
    pub pool: PgPool,
}

/// The job handler: runs the task, counts it, and on a failure moves the
/// job's `run_at` back by the backoff and returns a plain error, so apalis
/// tries it again (at most [`MAX_ATTEMPTS`] times in all). apalis fills each
/// argument from the job: the payload, the worker data and the job id.
///
/// Each job is a root `okf.housekeeping.job` span (`parent = None`: a job has
/// no request to belong to) carrying the task and the outcome.
#[tracing::instrument(
    level = "info",
    name = "okf.housekeeping.job",
    parent = None,
    skip_all,
    fields(
        okf.housekeeping.task = job.task.as_str(),
        okf.housekeeping.slot = job.slot,
        okf.store = job.store.as_ref().map(StoreName::as_str),
        okf.outcome = tracing::field::Empty,
    )
)]
async fn run_job(
    job: Housekeeping,
    context: Data<HousekeepingContext>,
    id: TaskId,
) -> Result<(), BoxDynError> {
    let started = std::time::Instant::now();
    let result = match &job.store {
        None => run(job.task, &context.app).await,
        Some(store) => run_on_store(job.task, store, &context.app).await,
    };
    let outcome = if result.is_ok() {
        Outcome::Ok
    } else {
        Outcome::Error
    };
    tracing::Span::current().record("okf.outcome", outcome.as_str());
    context
        .app
        .metrics()
        .housekeeping_run(job.task, outcome, started.elapsed());
    let Err(error) = result else {
        return Ok(());
    };
    split_event!(
        warn,
        { task = job.task.as_str(), store = job.store.as_ref().map_or("", StoreName::as_str), kind = error.kind(), },
        { %error, },
        "a housekeeping job failed; it is retried"
    );
    if let Err(reschedule) = reschedule_job(&context.pool, QUEUE, &id.to_string()).await {
        // Still a plain error: apalis fetches it again, just sooner.
        split_event!(
            error,
            { kind = reschedule.kind(), },
            { error = %reschedule, },
            "rescheduling a housekeeping job failed"
        );
    }
    // `Box::new(..)`: the boxed error trait object apalis expects.
    Err(Box::new(error))
}

/// Starts the housekeeping worker for a serving process (`http::serve_with`
/// calls it on every replica, beside [`tick_until`]).
///
/// # Arguments
/// * `app` - the shared state.
/// * `pool` - the process's worker pool; it stays open when the worker ends,
///   and its owner (`serve_with`) closes it once every worker has stopped.
/// * `shutdown` - cancelled when the server stops.
///
/// # Returns
/// The worker's task; it ends once `shutdown` is cancelled and the job in
/// hand (if any) is finished.
#[tracing::instrument(level = "debug", skip_all)]
pub fn spawn_worker(
    app: Arc<App>,
    pool: &WorkerPool,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    let context = HousekeepingContext {
        app,
        pool: pool.pool().clone(),
    };
    let timing = WorkerTiming {
        poll: POLL_INTERVAL,
        ..WorkerTiming::SERVE
    };
    run_worker(context, worker_name(), timing, shutdown)
}

/// How many housekeeping workers this process has started, for
/// [`worker_name`]. A `static` atomic: one counter for the whole process.
static WORKERS_STARTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// This worker's name: `okf-housekeeping-<host>-<pid>` for the process's
/// first, then `.2`, `.3`, ... for any later one (several `serve`s in one
/// test process). apalis holds an advisory lock on each registered name for
/// the life of the registering connection, so two live workers must never
/// share one.
///
/// # Returns
/// The name.
#[tracing::instrument(level = "trace", skip_all)]
fn worker_name() -> String {
    let base = process_worker_name(WORKER_PREFIX);
    match WORKERS_STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) {
        0 => base,
        earlier => format!("{base}.{}", earlier + 1),
    }
}

/// Runs one apalis worker on [`QUEUE`] until `shutdown`, restarting it
/// after `timing.restart` if it stops on an error (a lost database, say).
/// It leaves `context.pool` open.
///
/// # Arguments
/// * `context` - what the handler needs.
/// * `name` - the worker's name; a restart registers as
///   `<name>-<restarts>`, a row of its own.
/// * `timing` - the poll, heartbeat and restart delays.
/// * `shutdown` - cancelled to stop.
///
/// # Returns
/// The worker's task.
#[tracing::instrument(level = "debug", skip_all)]
pub(crate) fn run_worker(
    context: HousekeepingContext,
    name: String,
    timing: WorkerTiming,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        run_until_cancelled(&context, &name, timing, &shutdown).await;
    })
}

/// [`run_worker`]'s loop: returns once `shutdown` is cancelled.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "long-lived loop: its span would never close and would stay open as the parent of every later span"
    )
)]
async fn run_until_cancelled(
    context: &HousekeepingContext,
    name: &str,
    timing: WorkerTiming,
    shutdown: &CancellationToken,
) {
    // `0..`: an endless range, counting the restarts.
    for restarts in 0_u64.. {
        let config = QueueConfig::default()
            .queue(QUEUE)
            .heartbeat_interval(timing.heartbeat)
            .missed_heartbeats(timing.missed_heartbeats);
        // No `with_pubsub()`: the worker polls, so it holds no LISTEN
        // connection of the shared pool.
        let backend = PostgresStorage::<Housekeeping>::new(&context.pool)
            .with_config(config)
            .poll_with_interval(timing.poll);
        let registered = if restarts == 0 {
            name.to_string()
        } else {
            format!("{name}-{restarts}")
        };
        let worker = WorkerBuilder::new(registered)
            .backend(backend)
            .data(context.clone())
            .build(run_job);
        let stop = shutdown.clone();
        // The future apalis waits on: it resolves when we are cancelled.
        let signal = async move {
            stop.cancelled().await;
            Ok::<(), std::io::Error>(())
        };
        match worker.run_until(signal).await {
            Ok(()) => return,
            Err(error) => {
                split_event!(
                    error,
                    {},
                    { %error, },
                    "the housekeeping worker stopped; restarting it soon"
                );
            }
        }
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(timing.restart) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, app_with_metrics, local_metrics, render, sample_of};
    use crate::store::audit::AuditActor;
    use crate::store::catalogs;
    use crate::store::changesets::{ChangesetState, EntryRow};
    use crate::store::settings;
    use chrono::TimeZone;

    /// Generous: the tests share a loaded machine.
    pub(super) const PATIENCE: Duration = Duration::from_secs(30);

    /// A worker's timing scaled down for a test: it polls every 100 ms.
    const FAST: WorkerTiming = WorkerTiming {
        poll: Duration::from_millis(100),
        heartbeat: Duration::from_secs(1),
        restart: Duration::from_millis(200),
        missed_heartbeats: 6,
    };

    pub(super) fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 18, 19, 52, 10).unwrap()
    }

    /// The housekeeping jobs' `(idempotency_key, status, attempts)`, by key.
    pub(super) async fn jobs(pool: &PgPool) -> Vec<(String, String, i32)> {
        sqlx::query_as(
            "SELECT idempotency_key, status, attempts FROM apalis.jobs \
             WHERE job_type = $1 ORDER BY idempotency_key",
        )
        .bind(QUEUE)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// Waits until the job keyed `key` has `status`, or panics after
    /// [`PATIENCE`].
    pub(super) async fn wait_for(pool: &PgPool, key: &str, status: &str) {
        let waited = tokio::time::timeout(PATIENCE, async {
            loop {
                let found: Option<String> = sqlx::query_scalar(
                    "SELECT status FROM apalis.jobs WHERE job_type = $1 AND idempotency_key = $2",
                )
                .bind(QUEUE)
                .bind(key)
                .fetch_optional(pool)
                .await
                .unwrap();
                if found.as_deref() == Some(status) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        assert!(
            waited.is_ok(),
            "timed out waiting for job {key} to be {status}"
        );
    }

    /// A second "replica": its own pool on `test`'s database and its own
    /// instruments, read through the returned registry.
    async fn replica(
        test: &crate::mcp::testing::TestApp,
    ) -> (
        Arc<App>,
        opentelemetry_sdk::metrics::SdkMeterProvider,
        prometheus::Registry,
    ) {
        let (provider, metrics, registry) = local_metrics();
        let pool = crate::db::connect(test.db_url(), 5).await.unwrap();
        let app = App::load_with_metrics(pool, Config::default(), Arc::new(SystemClock), metrics)
            .await
            .unwrap();
        (app, provider, registry)
    }

    /// Starts `app`'s housekeeping worker with [`FAST`] timing, on a worker
    /// pool of its own named `name`.
    pub(super) fn start(
        app: &Arc<App>,
        name: &str,
    ) -> (CancellationToken, JoinHandle<()>, WorkerPool) {
        let pool = WorkerPool::new(app.accounts().executor());
        let context = HousekeepingContext {
            app: Arc::clone(app),
            pool: pool.pool().clone(),
        };
        let shutdown = CancellationToken::new();
        let worker = run_worker(context, name.to_string(), FAST, shutdown.clone());
        (shutdown, worker, pool)
    }

    /// Stops a worker started by [`start`] and closes its pool.
    pub(super) async fn stop(started: (CancellationToken, JoinHandle<()>, WorkerPool)) {
        let (shutdown, worker, pool) = started;
        shutdown.cancel();
        tokio::time::timeout(PATIENCE, worker)
            .await
            .expect("the worker stops")
            .unwrap();
        pool.close().await;
    }

    /// How many `task` jobs the registry counts with `outcome`.
    pub(super) fn runs(registry: &prometheus::Registry, task: Task, outcome: &str) -> f64 {
        sample_of(
            &render(registry),
            &format!(
                "okf_housekeeping_runs_total{{outcome=\"{outcome}\",task=\"{}\"}}",
                task.as_str()
            ),
        )
        .unwrap_or(0.0)
    }

    #[test]
    fn slots_start_at_their_cadence_boundary() {
        // 2026-11-18T19:52:10Z.
        let now = fixed_now();
        let at = |hour, minute| {
            Utc.with_ymd_and_hms(2026, 11, 18, hour, minute, 0)
                .unwrap()
                .timestamp()
        };

        assert_eq!(Task::PruneOauth.slot_at(now), at(19, 0));
        assert_eq!(Task::PruneWorkers.slot_at(now), at(19, 0));
        assert_eq!(Task::PruneChangesets.slot_at(now), at(19, 50));
        assert_eq!(Task::PruneAudit.slot_at(now), at(0, 0));
        assert_eq!(Task::ClusterStats.slot_at(now), at(19, 52));
        assert_eq!(Task::ProvisionCatalogs.slot_at(now), at(19, 52));
        // A window's start is its own slot, and the next second too.
        let start = Utc.timestamp_opt(at(19, 50), 0).unwrap();
        assert_eq!(Task::PruneChangesets.slot_at(start), at(19, 50));
        assert_eq!(
            Task::PruneChangesets.slot_at(start - TimeDelta::seconds(1)),
            at(19, 45)
        );
    }

    #[test]
    fn task_names_are_fixed_and_match_their_serialized_form() {
        let names: Vec<&str> = Task::ALL.into_iter().map(Task::as_str).collect();
        assert_eq!(
            names,
            [
                "prune_changesets",
                "prune_oauth",
                "prune_audit",
                "prune_workers",
                "cluster_stats",
                "provision_catalogs",
                "cleanup_stores"
            ]
        );
        for task in Task::ALL {
            assert_eq!(
                serde_json::to_value(task).unwrap(),
                serde_json::json!(task.as_str())
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_replicas_scheduling_one_slot_create_one_job() {
        let test = app_with(&[], Config::default()).await;
        let (other, _provider, _registry) = replica(&test).await;
        let slot = Task::PruneOauth.slot_at(fixed_now());

        // Both replicas push the slot, many times over and all at once: each
        // push is a task of its own (a `JoinSet` runs them concurrently).
        let mut pushes = tokio::task::JoinSet::new();
        for turn in 0..16 {
            let pool = if turn % 2 == 0 {
                test.app.accounts().executor().clone()
            } else {
                other.accounts().executor().clone()
            };
            pushes.spawn(async move {
                schedule(&pool, &Housekeeping::cluster(Task::PruneOauth, slot)).await
            });
        }
        let created = tokio::time::timeout(PATIENCE, pushes.join_all())
            .await
            .expect("every push returns");
        // The next slot is a job of its own.
        let next = slot + 3600;
        let next_created = schedule(
            test.app.accounts().executor(),
            &Housekeeping::cluster(Task::PruneOauth, next),
        )
        .await;

        let created: Vec<bool> = created
            .into_iter()
            .map(|pushed| pushed.expect("a duplicate is ignored, not an error"))
            .collect();
        assert_eq!(
            created.iter().filter(|made| **made).count(),
            1,
            "{created:?}"
        );
        assert!(next_created.unwrap());
        assert_eq!(
            jobs(test.app.accounts().executor()).await,
            [
                (format!("prune_oauth:{slot}"), "Pending".to_string(), 0),
                (format!("prune_oauth:{next}"), "Pending".to_string(), 0),
            ]
        );
    }

    /// Issue #748: a tick blocked on a lock does not hold shutdown up.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_ticker_stops_at_shutdown_while_a_tick_is_blocked() {
        let test = app_with(&[], Config::default()).await;
        // Arrange: another connection holds ACCESS EXCLUSIVE on the queue.
        let mut blocker = test.app.accounts().executor().begin().await.unwrap();
        sqlx::query("LOCK TABLE apalis.jobs IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await
            .unwrap();
        // The tests share one Postgres server, so the waiter below must be
        // blocked by this backend, not by another test's lock.
        let holder: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let ticker = tokio::spawn(tick_until(Arc::clone(&test.app), shutdown.clone()));
        // Wait until the tick's query waits on that lock.
        tokio::time::timeout(PATIENCE, async {
            loop {
                let waiting: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM pg_stat_activity \
                     WHERE datname = current_database() AND wait_event_type = 'Lock' \
                     AND query ILIKE '%apalis.jobs%' AND pid <> pg_backend_pid() \
                     AND pg_blocking_pids(pid) @> ARRAY[$1]::int[]",
                )
                .bind(holder)
                .fetch_one(test.app.accounts().executor())
                .await
                .unwrap();
                if waiting > 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the tick blocks on the lock");

        // Act and assert: cancelling ends the ticker promptly.
        shutdown.cancel();
        let stopped = tokio::time::timeout(Duration::from_secs(5), ticker).await;
        blocker.rollback().await.unwrap();
        stopped
            .expect("the ticker returns within the bound")
            .expect("the ticker does not panic");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_scheduled_slot_is_not_pushed_again() {
        let test = app_with(&[], Config::default()).await;
        let pool = test.app.accounts().executor().clone();
        let slot = Task::PruneOauth.slot_at(fixed_now());
        assert!(
            schedule(&pool, &Housekeeping::cluster(Task::PruneOauth, slot))
                .await
                .unwrap()
        );
        // From now on any insert into the jobs table fails loudly, so a push
        // that reached the insert would return an error, not `false`.
        sqlx::raw_sql(
            "CREATE FUNCTION refuse_job() RETURNS trigger LANGUAGE plpgsql AS \
             $$ BEGIN RAISE EXCEPTION 'insert attempted'; END $$; \
             CREATE TRIGGER refuse_job BEFORE INSERT ON apalis.jobs \
             FOR EACH ROW EXECUTE FUNCTION refuse_job();",
        )
        .execute(&pool)
        .await
        .unwrap();

        let again = schedule(&pool, &Housekeeping::cluster(Task::PruneOauth, slot)).await;
        let next = schedule(&pool, &Housekeeping::cluster(Task::PruneOauth, slot + 3600)).await;

        assert!(!again.expect("no insert is attempted"), "already scheduled");
        assert!(next.is_err(), "a new slot does reach the insert: {next:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_workers_racing_run_each_slot_once() {
        let (test, registry) = app_with_metrics(&[], Config::default()).await;
        let (other, _provider, other_registry) = replica(&test).await;
        let a = start(&test.app, "replica-a");
        let b = start(&other, "replica-b");
        let slots: Vec<i64> = (0..4).map(|minute| 60 * (1_000_000 + minute)).collect();

        // Both replicas schedule each slot while both workers poll.
        for slot in &slots {
            let job = Housekeeping::cluster(Task::ClusterStats, *slot);
            let (first, second) = tokio::join!(
                schedule(test.app.accounts().executor(), &job),
                schedule(other.accounts().executor(), &job),
            );
            assert!(first.unwrap() ^ second.unwrap(), "one push creates it");
        }
        for slot in &slots {
            wait_for(
                test.app.accounts().executor(),
                &format!("cluster_stats:{slot}"),
                "Done",
            )
            .await;
        }
        // Several polls more, for any second run to show.
        tokio::time::sleep(Duration::from_millis(500)).await;
        stop(a).await;
        stop(b).await;

        let total = runs(&registry, Task::ClusterStats, "ok")
            + runs(&other_registry, Task::ClusterStats, "ok");
        assert_eq!(total, 4.0, "each slot ran once in the cluster");
        assert!(jobs(test.app.accounts().executor())
            .await
            .iter()
            .all(|(_, status, attempts)| status == "Done" && *attempts == 1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_job_is_put_back_with_a_backoff() {
        let (test, registry) = app_with_metrics(&[], Config::default()).await;
        // The task's write fails while the table is gone.
        sqlx::query("DROP TABLE cluster_stats")
            .execute(test.app.accounts().executor())
            .await
            .unwrap();
        let worker = start(&test.app, "replica-a");
        schedule(
            test.app.accounts().executor(),
            &Housekeeping::cluster(Task::ClusterStats, 60),
        )
        .await
        .unwrap();

        wait_for(test.app.accounts().executor(), "cluster_stats:60", "Failed").await;
        stop(worker).await;

        let (attempts, max_attempts, waits): (i32, i32, f64) = sqlx::query_as(
            "SELECT attempts, max_attempts, extract(epoch FROM run_at - now())::float8 \
             FROM apalis.jobs WHERE job_type = $1",
        )
        .bind(QUEUE)
        .fetch_one(test.app.accounts().executor())
        .await
        .unwrap();
        assert_eq!((attempts, max_attempts), (1, 3), "it is tried again");
        assert!(
            waits > 15.0,
            "after the backoff, not at the next poll: {waits}"
        );
        assert_eq!(runs(&registry, Task::ClusterStats, "error"), 1.0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cluster_stats_are_reported_by_every_replica() {
        let (test, registry) =
            app_with_metrics(&[("pg", &[("a.md", "# A\n")])], Config::default()).await;
        let (other, _provider, other_registry) = replica(&test).await;
        let pg = catalogs::find(test.app.accounts().executor(), "pg")
            .await
            .unwrap();
        writer_key(test.app.accounts().executor()).await;
        changeset_at(test.app.accounts().executor(), pg.id, Utc::now(), None).await;
        changeset_at(test.app.accounts().executor(), pg.id, Utc::now(), None).await;
        crate::mail::queue::enqueue(
            test.app.accounts().executor(),
            "alice@example.com",
            &crate::mail::Template::Test,
            Utc::now(),
            &crate::store::settings::Brand::default(),
        )
        .await
        .unwrap();
        let open =
            |registry: &prometheus::Registry| sample_of(&render(registry), "okf_changesets_open");
        let pending = |registry: &prometheus::Registry| {
            sample_of(
                &render(registry),
                "okf_mail_queue_depth{status=\"pending\"}",
            )
        };
        // Before any row: present from the first scrape, at zero.
        refresh_gauges(&other).await;
        let before = (open(&other_registry), pending(&other_registry));

        // One replica's jobs (the store's count, then the cluster's sum)
        // write the row; each replica reads it.
        run_on_store(Task::ClusterStats, &StoreName::main(), &test.app)
            .await
            .unwrap();
        run(Task::ClusterStats, &test.app).await.unwrap();
        let unread = open(&registry);
        refresh_gauges(&test.app).await;
        refresh_gauges(&other).await;

        assert_eq!(before, (Some(0.0), Some(0.0)));
        assert_eq!(unread, Some(0.0), "a scrape reads no rows");
        for registry in [&registry, &other_registry] {
            assert_eq!(open(registry), Some(2.0), "{}", render(registry));
            assert_eq!(pending(registry), Some(1.0), "{}", render(registry));
        }
        let stats = read_stats(test.app.accounts().executor())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stats.open_changesets, 2);
        assert!(stats.mail_queue.contains(&(MailStatus::Pending, 1)));
        assert!(stats.mail_queue.contains(&(MailStatus::Failed, 0)));
    }

    /// Creates key `writer`, which [`changeset_at`] opens changesets for.
    pub(super) async fn writer_key(pool: &PgPool) {
        keys::insert_legacy_test_key(pool, "writer", keys::Access::Read, "tester", Utc::now())
            .await;
    }

    /// Opens a changeset for key `writer` at `at`, with an entry for `path`
    /// if given.
    async fn changeset_at(
        pool: &PgPool,
        catalog_id: i64,
        at: DateTime<Utc>,
        path: Option<&str>,
    ) -> i64 {
        changeset_on(&CatalogDb::main_of(pool), catalog_id, at, path).await
    }

    /// [`changeset_at`] on the store `db`.
    pub(super) async fn changeset_on(
        db: &CatalogDb,
        catalog_id: i64,
        at: DateTime<Utc>,
        path: Option<&str>,
    ) -> i64 {
        let mut tx = db.begin().await.unwrap();
        changesets::lock_catalog(&mut tx, catalog_id).await.unwrap();
        let changeset = changesets::open(&mut tx, catalog_id, "writer", "writer", at)
            .await
            .unwrap();
        if let Some(path) = path {
            let entry = EntryRow {
                path: path.to_string(),
                base_revision: None,
                content: Some(b"x\n".to_vec()),
                executable: false,
                replay: None,
            };
            changesets::upsert_entry(&mut tx, changeset.id, &entry, at)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        changeset.id
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_changesets_discards_empty_changesets_a_day_old_and_keeps_the_rest() {
        let test = app_with(&[("pg", &[("a.md", "# A\n")])], Config::default()).await;
        let pool = test.app.accounts().executor().clone();
        let pg = catalogs::find(&pool, "pg").await.unwrap();
        writer_key(&pool).await;
        let day_old = Utc::now() - TimeDelta::hours(25);
        let empty_old = changeset_at(&pool, pg.id, day_old, None).await;
        let empty_new = changeset_at(&pool, pg.id, Utc::now(), None).await;
        let full_old = changeset_at(&pool, pg.id, day_old, Some("a.md")).await;

        run_on_store(Task::PruneChangesets, &StoreName::main(), &test.app)
            .await
            .unwrap();

        let store = test.app.main_db().await;
        let state = |id| {
            let store = store.clone();
            async move {
                changesets::by_id(store.executor(), id)
                    .await
                    .unwrap()
                    .unwrap()
                    .state
            }
        };
        assert_eq!(state(empty_old).await, ChangesetState::Discarded);
        assert_eq!(state(empty_new).await, ChangesetState::Open);
        assert_eq!(state(full_old).await, ChangesetState::Open);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn provision_catalogs_finishes_or_removes_old_half_made_catalogs_and_spares_young_ones() {
        let test = app_with(&[("whole", &[])], Config::default()).await;
        let pool = test.app.accounts().executor().clone();
        let now = test.app.clock().now();
        let owner = crate::db::TEST_TEAM;
        // `finish` has its store side but no `provisioned_at`; `lost` has
        // neither; `young` has neither and is too new to look at.
        for name in ["finish", "lost", "young"] {
            let name_text = name.to_string();
            let display = crate::labels::DisplayName::default();
            catalogs::create(
                &pool,
                &catalogs::NewCatalog {
                    name: &name_text,
                    display_name: &display,
                    description: "",
                    owner_team: owner,
                },
                &AuditActor::cli(),
                now,
            )
            .await
            .unwrap();
        }
        sqlx::query(
            "UPDATE catalogs SET provisioned_at = NULL, created_at = $1 \
             WHERE name IN ('finish', 'lost')",
        )
        .bind(now - TimeDelta::minutes(5))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE catalogs SET provisioned_at = NULL WHERE name = 'young'")
            .execute(&pool)
            .await
            .unwrap();
        let gone = [
            crate::db::catalog_id(&pool, "lost").await,
            crate::db::catalog_id(&pool, "young").await,
        ];
        sqlx::query("DELETE FROM catalog.catalog_state WHERE id = ANY($1)")
            .bind(&gone[..])
            .execute(&crate::db::store_pool(&pool))
            .await
            .unwrap();

        run(Task::ProvisionCatalogs, &test.app).await.unwrap();

        let rows: Vec<(String, bool)> =
            sqlx::query_as("SELECT name, provisioned_at IS NOT NULL FROM catalogs ORDER BY name")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            [
                ("finish".to_string(), true),
                ("whole".to_string(), true),
                ("young".to_string(), false),
            ]
        );
        let removed = crate::store::audit::testing::entries(&pool, "catalog.deleted").await;
        assert_eq!(removed.len(), 1, "{removed:?}");
        assert_eq!(removed[0].target.as_deref(), Some("lost"));
        assert_eq!(removed[0].details["failed_create"], true);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_oauth_deletes_stale_oauth_rows_and_old_housekeeping_jobs() {
        use crate::store::keys::Access;
        use crate::store::oauth::testing::{insert_client, insert_grant};
        let test = app_with(&[("main", &[])], Config::default()).await;
        let pool = test.app.accounts().executor().clone();
        let now = Utc::now();
        let client = insert_client(
            &pool,
            "https://client.example.com/client.json",
            "Example",
            now,
        )
        .await;
        let grant = insert_grant(&pool, &client, Access::Read, &["main"], now).await;
        sqlx::query(
            "INSERT INTO oauth_tokens \
             (sha256, grant_id, kind, family_id, resource, created_at, expires_at) \
             VALUES ($1, $2, 'access', 'f', 'https://okf.example.com/mcp', $3, $4)",
        )
        .bind([1u8; 32].as_slice())
        .bind(&grant)
        .bind(now - TimeDelta::days(9))
        .bind(now - TimeDelta::days(8))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO oauth_clients (client_id, kind, redirect_uris, metadata, created_at) \
             VALUES ('unbound', 'dcr', ARRAY['https://client.example.com/callback'], '{}', $1)",
        )
        .bind(now - TimeDelta::days(2))
        .execute(&pool)
        .await
        .unwrap();
        // Housekeeping jobs: done two days and an hour ago, one given up on
        // two days ago, one pending.
        for (slot, status, done_hours_ago) in [
            (1, "Done", Some(48)),
            (2, "Done", Some(1)),
            (3, "Killed", Some(48)),
            (4, "Pending", None),
        ] {
            schedule(&pool, &Housekeeping::cluster(Task::ClusterStats, slot))
                .await
                .unwrap();
            sqlx::query(
                "UPDATE apalis.jobs SET status = $2, done_at = $3 \
                 WHERE job_type = $4 AND idempotency_key = $1",
            )
            .bind(format!("cluster_stats:{slot}"))
            .bind(status)
            .bind(done_hours_ago.map(|hours| now - TimeDelta::hours(hours)))
            .bind(QUEUE)
            .execute(&pool)
            .await
            .unwrap();
        }
        let count = |table: &'static str| {
            let pool = pool.clone();
            async move {
                // `AssertSqlSafe`: `table` is one of this test's literals.
                sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
                    "SELECT count(*) FROM {table}"
                )))
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };

        run(Task::PruneOauth, &test.app).await.unwrap();

        assert_eq!(count("oauth_tokens").await, 0);
        assert_eq!(count("oauth_clients").await, 1, "the bound client stays");
        let kept: Vec<String> = jobs(&pool).await.into_iter().map(|(key, ..)| key).collect();
        assert_eq!(
            kept,
            ["cluster_stats:2", "cluster_stats:3", "cluster_stats:4"],
            "a day of finished jobs and a month of failed ones are kept"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_oauth_revokes_grants_idle_past_the_configured_period() {
        use crate::store::keys::Access;
        use crate::store::oauth::testing::{insert_client, insert_grant};
        // 10 days instead of the default 30, so a grant last used 15 days
        // ago is idle here and would not be by default.
        let mut config = Config::default();
        config.oauth.idle_grant_revoke_after = Duration::from_secs(10 * 24 * 60 * 60);
        let test = app_with(&[("main", &[])], config).await;
        let pool = test.app.accounts().executor().clone();
        let now = Utc::now();
        let client = insert_client(
            &pool,
            "https://client.example.com/client.json",
            "Example",
            now,
        )
        .await;
        let created = now - TimeDelta::days(40);
        let grant_used = |days: i64, byte: u8| {
            let pool = pool.clone();
            let client = client.clone();
            async move {
                let grant = insert_grant(&pool, &client, Access::Read, &["main"], created).await;
                sqlx::query(
                    "INSERT INTO oauth_tokens \
                     (sha256, grant_id, kind, family_id, resource, created_at, expires_at) \
                     VALUES ($1, $2, 'access', 'f', 'https://okf.example.com/mcp', $3, \
                             $3 + interval '1 hour')",
                )
                .bind([byte; 32].as_slice())
                .bind(&grant)
                .bind(now - TimeDelta::days(days))
                .execute(&pool)
                .await
                .unwrap();
                grant
            }
        };
        let idle = grant_used(15, 1).await;
        let recent = grant_used(5, 2).await;

        run(Task::PruneOauth, &test.app).await.unwrap();

        let revoked = |id: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
                    "SELECT revoked_at FROM api_keys WHERE id = $1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
                .is_some()
            }
        };
        assert!(revoked(idle).await);
        assert!(!revoked(recent).await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_oauth_deletes_old_sign_in_events_and_long_expired_emailed_links() {
        let test = app_with(&[("main", &[])], Config::default()).await;
        let pool = test.app.accounts().executor().clone();
        let now = Utc::now();
        let user: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(crate::db::TEST_USER)
            .fetch_one(&pool)
            .await
            .unwrap();
        for (byte, expired_days_ago) in [(1u8, 8), (2u8, 1)] {
            sqlx::query(
                "INSERT INTO email_tokens (hash, user_id, purpose, created_at, expires_at) \
                 VALUES ($1, $2, 'setup', $3, $4)",
            )
            .bind([byte; 32].as_slice())
            .bind(&user)
            .bind(now - TimeDelta::days(expired_days_ago + 1))
            .bind(now - TimeDelta::days(expired_days_ago))
            .execute(&pool)
            .await
            .unwrap();
        }
        for days_ago in [91, 1] {
            sqlx::query(
                "INSERT INTO sign_in_events (user_id, method, outcome, at) \
                 VALUES ($1, 'password', 'failed', $2)",
            )
            .bind(&user)
            .bind(now - TimeDelta::days(days_ago))
            .execute(&pool)
            .await
            .unwrap();
        }

        run(Task::PruneOauth, &test.app).await.unwrap();

        let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM email_tokens")
            .fetch_one(&pool)
            .await
            .unwrap();
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM sign_in_events")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((tokens, events), (1, 1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_audit_deletes_entries_past_the_retention_setting() {
        use crate::store::audit::{Action, Entry, Target};
        let test = app_with(&[("main", &[])], Config::default()).await;
        let pool = test.app.accounts().executor().clone();
        settings::set(
            &pool,
            settings::SettingKey::AuditRetentionDays,
            "60",
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        // The running app learns of the setting as a server would.
        test.app.reload_settings().await;
        for days in [61, 59] {
            let mut tx = pool.begin().await.unwrap();
            audit::record(
                &mut tx,
                &AuditActor::cli(),
                &Entry {
                    action: Action::SettingCleared,
                    target: Target::Setting("product_name"),
                    person: None,
                    teams: &[],
                    details: serde_json::json!({}),
                },
                Utc::now() - TimeDelta::days(days),
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }

        // A provisioner budget counter from three hours ago, well inside the
        // retention period.
        sqlx::query("INSERT INTO audit_access_counts (bucket, requests) VALUES ($1, 1)")
            .bind(format!(
                "provcap\u{1f}lapi key\u{1f}{}",
                (Utc::now() - TimeDelta::hours(3)).timestamp()
            ))
            .execute(&pool)
            .await
            .unwrap();

        run(Task::PruneAudit, &test.app).await.unwrap();

        let left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_entries WHERE action = 'setting.cleared'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(left, 1, "61 days is past 60 (plus the hour); 59 is not");
        let counters: i64 =
            sqlx::query_scalar("SELECT count(*) FROM audit_access_counts WHERE entry_id IS NULL")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(counters, 0, "a past hour's budget counter goes too");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_workers_deletes_a_stale_worker_row_and_keeps_a_live_one() {
        let test = app_with(&[], Config::default()).await;
        let pool = test.app.accounts().executor().clone();
        for (id, seen) in [("stale", TimeDelta::hours(1)), ("live", TimeDelta::zero())] {
            sqlx::query(
                "INSERT INTO apalis.workers (id, worker_type, storage_name, layers, last_seen) \
                 VALUES ($1, $2, 'PostgresStorage', '', $3)",
            )
            .bind(id)
            .bind(QUEUE)
            .bind(Utc::now() - seen)
            .execute(&pool)
            .await
            .unwrap();
        }

        run(Task::PruneWorkers, &test.app).await.unwrap();

        let left: Vec<String> = sqlx::query_scalar("SELECT id FROM apalis.workers ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(left, ["live"]);
    }
}

#[cfg(test)]
mod store_tests;

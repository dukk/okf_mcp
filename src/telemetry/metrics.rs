//! The server's metric instruments, behind one typed handle.
//!
//! Callers record through [`Metrics`]'s methods only: never an instrument,
//! never a raw attribute. Every label value is a closed set (an enum's
//! `as_str`, or [`tool_label`]'s allow-list), so a series count cannot grow
//! with what a client sends (spec "The `Metrics` handle").

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter, UpDownCounter};
use opentelemetry::KeyValue;

use super::otlp::{ExportFailures, FailureReason, Signal};
use crate::app::App;
use crate::config::MailProvider;
use crate::mcp::annotations::{DESTRUCTIVE, READS, STAGED};
use crate::store::sign_in_events::{Method, Outcome as SignInOutcome};

/// Seconds-histogram boundaries: 1 ms to 60 s. The SDK's defaults are
/// scaled for milliseconds (plan "Verified facts").
pub const SECONDS_BUCKETS: [f64; 14] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 60.0,
];

/// What an operation ended as: a closed set, so a label never grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The operation succeeded.
    Ok,
    /// The operation failed.
    Error,
}

impl Outcome {
    /// The label value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }
}

/// The server's instruments.
///
/// Cheap to clone: the instruments sit behind an `Arc`, so [`crate::app::App`]
/// and any callback share one set.
#[derive(Debug, Clone)]
pub struct Metrics {
    // Tasks 3-6 add one field per instrument to `Instruments`, and the typed
    // methods that read them.
    inner: Arc<Instruments>,
    // Observable gauges are registered after the `App` exists, on this meter.
    meter: Meter,
}

/// Every instrument, built once from a [`Meter`].
#[derive(Debug)]
struct Instruments {
    // Task 3: HTTP and MCP.
    http_duration: Histogram<f64>,
    http_active: UpDownCounter<i64>,
    tool_duration: Histogram<f64>,
    mcp_sessions: UpDownCounter<i64>,
    // Auth, rate limits and mail (Task 5).
    auth_attempts: Counter<u64>,
    oauth_tokens_issued: Counter<u64>,
    ratelimit_rejections: Counter<u64>,
    mail_jobs: Counter<u64>,
    mail_queue_depth: Arc<MailQueueDepth>,
    // Clock checks and archives (Task 6).
    clock_checks: Counter<u64>,
    clock_offset: Gauge<f64>,
    archive_size: Counter<u64>,
    archive_duration: Histogram<f64>,
}

impl Metrics {
    /// The Prometheus family names that exist before any traffic: the
    /// observable ones, which report from the first scrape. [`Metrics::new`]
    /// and [`observe_export_failures`] together make them present. Tasks 4-6
    /// add the catalog, changeset, mail-queue, pool and clock gauges.
    pub const FAMILIES: &'static [&'static str] = &[
        "okf_build_info",
        "okf_telemetry_export_failures_total",
        "okf_mail_queue_depth",
    ];
    // The pool gauge joins them once `App::load_with_metrics` registers it
    // (`observe_pool`).

    /// Builds every instrument on `meter` and records `okf.build.info`: a
    /// gauge fixed at 1 whose `version` label is the crate version.
    ///
    /// # Arguments
    /// * `meter` - the meter to build on.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn new(meter: &Meter) -> Self {
        meter
            .u64_gauge("okf.build.info")
            .with_description("The running build; always 1.")
            .build()
            .record(1, &[KeyValue::new("version", env!("CARGO_PKG_VERSION"))]);
        Self {
            inner: Arc::new(Instruments {
                http_duration: seconds_histogram(
                    meter,
                    "http.server.request.duration",
                    "HTTP server request duration, by method, route and status.",
                ),
                http_active: meter
                    .i64_up_down_counter("http.server.active_requests")
                    .with_description("HTTP requests being served, by method and route.")
                    .build(),
                tool_duration: seconds_histogram(
                    meter,
                    "okf.mcp.tool.duration",
                    "MCP tool call duration, by tool, catalog and outcome.",
                ),
                mcp_sessions: meter
                    .i64_up_down_counter("okf.mcp.sessions.active")
                    .with_description("Open MCP sessions.")
                    .build(),
                auth_attempts: counter(meter, "okf.auth.attempts", "Sign-in attempts."),
                oauth_tokens_issued: counter(
                    meter,
                    "okf.oauth.tokens.issued",
                    "OAuth token pairs issued, by grant.",
                ),
                ratelimit_rejections: counter(
                    meter,
                    "okf.ratelimit.rejections",
                    "Requests a rate limiter refused.",
                ),
                mail_jobs: counter(
                    meter,
                    "okf.mail.jobs",
                    "Mail jobs, by provider and outcome.",
                ),
                mail_queue_depth: MailQueueDepth::observe(meter),
                clock_checks: counter(meter, "okf.clock.checks", "Clock checks, by outcome."),
                clock_offset: meter
                    .f64_gauge("okf.clock.offset")
                    .with_unit("s")
                    .with_description("The time server's clock minus ours, at the last check.")
                    .build(),
                archive_size: meter
                    .u64_counter("okf.archive.size")
                    .with_unit("By")
                    .with_description("Archive bytes moved, by direction and catalog.")
                    .build(),
                archive_duration: seconds_histogram(
                    meter,
                    "okf.archive.duration",
                    "Archive exports and imports, by direction and outcome.",
                ),
            }),
            meter: meter.clone(),
        }
    }

    /// The instruments on the global meter provider: a no-op until `serve`
    /// sets one, so tests and the other subcommands record nothing.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn global() -> Self {
        Self::new(&opentelemetry::global::meter("okf-mcp"))
    }
}

/// Task 3: the HTTP and MCP instruments' typed methods.
impl Metrics {
    /// Records one finished HTTP request.
    ///
    /// # Arguments
    /// * `method` - the semconv method (`GET`, ... or `_OTHER`).
    /// * `route` - the matched route template; never the literal path.
    /// * `status` - the response status code.
    /// * `elapsed` - how long the request took.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn http_request(&self, method: &'static str, route: &str, status: u16, elapsed: Duration) {
        self.inner.http_duration.record(
            elapsed.as_secs_f64(),
            &[
                KeyValue::new("http.request.method", method),
                KeyValue::new("http.route", route.to_string()),
                KeyValue::new("http.response.status_code", i64::from(status)),
            ],
        );
    }

    /// Counts a request as in flight until the returned guard drops
    /// (including when the request's future is cancelled).
    ///
    /// # Arguments
    /// * `method` - the semconv method.
    /// * `route` - the matched route template.
    ///
    /// # Returns
    /// The guard; dropping it decrements the counter.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn http_in_flight(&self, method: &'static str, route: &str) -> InFlight {
        let attributes = vec![
            KeyValue::new("http.request.method", method),
            KeyValue::new("http.route", route.to_string()),
        ];
        self.inner.http_active.add(1, &attributes);
        InFlight {
            counter: self.inner.http_active.clone(),
            attributes,
        }
    }

    /// Records one finished MCP tool call.
    ///
    /// # Arguments
    /// * `tool` - a [`tool_label`] value.
    /// * `catalog` - a loaded catalog's name, or `none`.
    /// * `outcome` - how the call ended.
    /// * `elapsed` - how long it took.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn tool_call(
        &self,
        tool: &'static str,
        catalog: &str,
        outcome: Outcome,
        elapsed: Duration,
    ) {
        self.inner.tool_duration.record(
            elapsed.as_secs_f64(),
            &[
                KeyValue::new("tool", tool),
                KeyValue::new("catalog", catalog.to_string()),
                KeyValue::new("outcome", outcome.as_str()),
            ],
        );
    }

    /// Counts an MCP session as open until the returned guard drops.
    ///
    /// # Returns
    /// The guard; dropping it decrements `okf.mcp.sessions.active`.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn mcp_session(&self) -> SessionGuard {
        self.inner.mcp_sessions.add(1, &[]);
        SessionGuard {
            counter: self.inner.mcp_sessions.clone(),
        }
    }
}

/// An HTTP request in flight; decrements `http.server.active_requests` when
/// dropped.
#[derive(Debug)]
pub struct InFlight {
    counter: UpDownCounter<i64>,
    attributes: Vec<KeyValue>,
}

// `Drop` runs when the value goes out of scope, like a Python context
// manager's `__exit__`, and also when a cancelled future is dropped.
impl Drop for InFlight {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "a drop hook that only adds to one counter"
        )
    )]
    fn drop(&mut self) {
        self.counter.add(-1, &self.attributes);
    }
}

/// An open MCP session; decrements `okf.mcp.sessions.active` when dropped.
#[derive(Debug)]
pub struct SessionGuard {
    counter: UpDownCounter<i64>,
}

impl Drop for SessionGuard {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "a drop hook that only adds to one counter"
        )
    )]
    fn drop(&mut self) {
        self.counter.add(-1, &[]);
    }
}

/// A counter of events (`okf.auth.attempts` renders as
/// `okf_auth_attempts_total`).
#[tracing::instrument(level = "trace", skip_all)]
fn counter(meter: &Meter, name: &'static str, description: &'static str) -> Counter<u64> {
    meter
        .u64_counter(name)
        .with_description(description)
        .build()
}

/// The rate limiters, a closed set: the `limiter` label of
/// `okf.ratelimit.rejections`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limiter {
    /// Password, passkey and OIDC sign-in starts, by address.
    SignInPerIp,
    /// `POST /signin/forgot`, by normalized email.
    ForgotPerEmail,
    /// `POST /signin/forgot`, by address.
    ForgotPerIp,
    /// A signed-in user's OIDC link starts.
    LinkStarts,
    /// OAuth dynamic client registration.
    OauthRegister,
    /// OAuth token and revocation endpoints.
    OauthToken,
    /// OAuth consent posts.
    OauthConsent,
}

impl Limiter {
    /// The label value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SignInPerIp => "sign_in_per_ip",
            Self::ForgotPerEmail => "forgot_per_email",
            Self::ForgotPerIp => "forgot_per_ip",
            Self::LinkStarts => "link_starts",
            Self::OauthRegister => "oauth_register",
            Self::OauthToken => "oauth_token",
            Self::OauthConsent => "oauth_consent",
        }
    }
}

/// The OAuth grants the token endpoint serves: the `grant` label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    /// `grant_type=authorization_code`.
    AuthorizationCode,
    /// `grant_type=refresh_token`.
    RefreshToken,
}

impl Grant {
    /// The label value, the grant type's own spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthorizationCode => "authorization_code",
            Self::RefreshToken => "refresh_token",
        }
    }
}

/// How a mail job ended: the `outcome` label of `okf.mail.jobs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailOutcome {
    /// The provider accepted the mail.
    Sent,
    /// A temporary failure; the job is scheduled again.
    Retry,
    /// A permanent failure, or the retry window ran out.
    Failed,
}

impl MailOutcome {
    /// The label value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Retry => "retry",
            Self::Failed => "failed",
        }
    }
}

/// Which way an archive moved: the `direction` label of the archive
/// instruments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// A catalog exported as a tarball.
    Export,
    /// A tarball imported into a catalog.
    Import,
}

impl Direction {
    /// The label value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Export => "export",
            Self::Import => "import",
        }
    }
}

/// The states a queued mail is shown in (`mail queue`): the `status` label
/// of `okf.mail.queue.depth`. Done jobs are not counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailStatus {
    /// Waiting for its first try.
    Pending,
    /// Claimed by a worker.
    Sending,
    /// Failed and scheduled again.
    Retrying,
    /// Given up on, until someone retries it.
    Failed,
}

impl MailStatus {
    /// Every status, in the order the gauge keeps them.
    pub const ALL: [Self; 4] = [Self::Pending, Self::Sending, Self::Retrying, Self::Failed];

    /// The label value, the spelling `mail::queue::list` gives.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sending => "sending",
            Self::Retrying => "retrying",
            Self::Failed => "failed",
        }
    }

    /// The status for a stored spelling, `None` for anything else.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.as_str() == text)
    }

    /// The slot this status keeps its count in.
    const fn slot(self) -> usize {
        match self {
            Self::Pending => 0,
            Self::Sending => 1,
            Self::Retrying => 2,
            Self::Failed => 3,
        }
    }
}

/// The mail queue's depth by status, as the last poll counted it. A scrape
/// reads these numbers; it never queries Postgres.
#[derive(Debug, Default)]
struct MailQueueDepth {
    counts: [AtomicI64; MailStatus::ALL.len()],
}

impl MailQueueDepth {
    /// Registers the `okf.mail.queue.depth` gauge on `meter`: one series per
    /// status from the first scrape (zero until a poll counts).
    #[tracing::instrument(level = "trace", skip_all)]
    fn observe(meter: &Meter) -> Arc<Self> {
        let depth = Arc::new(Self::default());
        let read = Arc::clone(&depth);
        // The instrument handle may be dropped: the callback stays
        // registered with the meter provider.
        meter
            .i64_observable_gauge("okf.mail.queue.depth")
            .with_description("Mail jobs not yet sent, by status, as of the last poll.")
            .with_callback(move |observer| {
                for status in MailStatus::ALL {
                    observer.observe(
                        read.counts[status.slot()].load(Ordering::Relaxed),
                        &[KeyValue::new("status", status.as_str())],
                    );
                }
            })
            .build();
        depth
    }
}

impl Metrics {
    /// Counts one sign-in attempt, at the two choke points that record a
    /// `sign_in_events` row. The labels are the audit's own closed enums.
    ///
    /// # Arguments
    /// * `method` - how they tried.
    /// * `outcome` - how it ended.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn auth_attempt(&self, method: Method, outcome: SignInOutcome) {
        self.inner.auth_attempts.add(
            1,
            &[
                KeyValue::new("method", method.as_str()),
                KeyValue::new("outcome", outcome.as_str()),
            ],
        );
    }

    /// Counts one OAuth token pair issued.
    ///
    /// # Arguments
    /// * `grant` - the grant that issued it.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn oauth_token_issued(&self, grant: Grant) {
        self.inner
            .oauth_tokens_issued
            .add(1, &[KeyValue::new("grant", grant.as_str())]);
    }

    /// Counts one request a rate limiter refused.
    ///
    /// # Arguments
    /// * `limiter` - which limiter.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn rate_limited(&self, limiter: Limiter) {
        self.inner
            .ratelimit_rejections
            .add(1, &[KeyValue::new("limiter", limiter.as_str())]);
    }

    /// Counts one mail job's try.
    ///
    /// # Arguments
    /// * `provider` - the configured provider.
    /// * `outcome` - how the try ended.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn mail_job(&self, provider: MailProvider, outcome: MailOutcome) {
        self.inner.mail_jobs.add(
            1,
            &[
                KeyValue::new("provider", provider.as_str()),
                KeyValue::new("outcome", outcome.as_str()),
            ],
        );
    }

    /// Stores the mail queue's depth for the gauge: the count per status
    /// the poll just read. A status missing from `counts` is zero.
    ///
    /// # Arguments
    /// * `counts` - `(status, jobs)` pairs.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn set_mail_queue_depth(&self, counts: &[(MailStatus, i64)]) {
        for status in MailStatus::ALL {
            let jobs = counts
                .iter()
                .filter(|(counted, _)| *counted == status)
                .map(|(_, jobs)| *jobs)
                .sum();
            self.inner.mail_queue_depth.counts[status.slot()].store(jobs, Ordering::Relaxed);
        }
    }
}

/// Task 6: pool, clock and archive instruments.
impl Metrics {
    /// Registers `db.client.connection.count`: the pool's connections, one
    /// series per `state` (`idle` and `used`, which sum to the pool's size).
    /// The callback holds only a [`Weak`] handle and reads the pool's own
    /// counters, so a scrape never queries Postgres.
    ///
    /// # Arguments
    /// * `app` - the app whose pool to read; a callback finding it dropped
    ///   reports nothing.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn observe_pool(&self, app: Weak<App>) {
        // The gauge handle may be dropped: the callback stays registered
        // with the provider (see `observe_export_failures`).
        self.meter
            .i64_observable_gauge("db.client.connection.count")
            .with_description("Postgres pool connections, by state.")
            .with_callback(move |observer| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                let pool = app.pool();
                let size = i64::from(pool.size());
                // `size` and `num_idle` are read a moment apart, so idle can
                // briefly exceed size; clamp, so used is never negative and
                // the two always sum to `size`.
                let idle = i64::try_from(pool.num_idle()).unwrap_or(i64::MAX).min(size);
                observer.observe(idle, &[KeyValue::new("state", "idle")]);
                observer.observe(size - idle, &[KeyValue::new("state", "used")]);
            })
            .build();
    }

    /// Records one clock check.
    ///
    /// # Arguments
    /// * `outcome` - `Ok` when the time server gave a usable reply.
    /// * `offset_ms` - the server's clock minus ours; `None` on failure,
    ///   which leaves the offset gauge at its last value.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn clock_check(&self, outcome: Outcome, offset_ms: Option<i64>) {
        self.inner
            .clock_checks
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
        if let Some(offset_ms) = offset_ms {
            // An offset is milliseconds well under 2^53: exact as an `f64`.
            #[allow(clippy::cast_precision_loss)]
            self.inner
                .clock_offset
                .record(offset_ms as f64 / 1000.0, &[]);
        }
    }

    /// Records a finished archive export or import of a catalog that exists.
    ///
    /// # Arguments
    /// * `direction` - export or import.
    /// * `catalog` - a loaded catalog's name (never a raw path segment).
    /// * `outcome` - how it ended.
    /// * `elapsed` - how long it took.
    /// * `bytes` - the archive's size when it succeeded; `None` adds no bytes.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn archive(
        &self,
        direction: Direction,
        catalog: &str,
        outcome: Outcome,
        elapsed: Duration,
        bytes: Option<u64>,
    ) {
        self.inner.archive_duration.record(
            elapsed.as_secs_f64(),
            &[
                KeyValue::new("direction", direction.as_str()),
                KeyValue::new("outcome", outcome.as_str()),
            ],
        );
        if let Some(bytes) = bytes {
            self.inner.archive_size.add(
                bytes,
                &[
                    KeyValue::new("direction", direction.as_str()),
                    KeyValue::new("catalog", catalog.to_owned()),
                ],
            );
        }
    }
}

/// Registers `okf.telemetry.export.failures` (rendered as
/// `okf_telemetry_export_failures_total`): an observable counter reading
/// `failures` at each collection, one series per `(signal, reason)`, every
/// one present from the first scrape (zero until a failure).
///
/// # Arguments
/// * `meter` - the meter to register on.
/// * `failures` - the counts the counting exporters keep.
#[tracing::instrument(level = "trace", skip_all)]
pub fn observe_export_failures(meter: &Meter, failures: Arc<ExportFailures>) {
    // The instrument handle may be dropped: the callback stays registered
    // with the meter provider.
    meter
        .u64_observable_counter("okf.telemetry.export.failures")
        .with_description("OTLP exports that failed, by signal and reason.")
        .with_callback(move |observer| {
            for signal in Signal::ALL {
                for reason in FailureReason::ALL {
                    observer.observe(
                        failures.count(signal, reason),
                        &[
                            KeyValue::new("signal", signal.label()),
                            KeyValue::new("reason", reason.label()),
                        ],
                    );
                }
            }
        })
        .build();
}

/// A histogram of durations in seconds, with [`SECONDS_BUCKETS`].
///
/// # Arguments
/// * `meter` - the meter to build on.
/// * `name` - the instrument name (`okf.mcp.tool.duration`); the `s` unit
///   makes Prometheus name it `..._seconds`.
/// * `description` - the `# HELP` text.
#[tracing::instrument(level = "trace", skip_all)]
pub fn seconds_histogram(
    meter: &Meter,
    name: &'static str,
    description: &'static str,
) -> Histogram<f64> {
    meter
        .f64_histogram(name)
        .with_unit("s")
        .with_description(description)
        .with_boundaries(SECONDS_BUCKETS.to_vec())
        .build()
}

/// The `tool` label for an MCP tool name: the name itself when it is one of
/// the server's registered tools, else `other`. A client can send any name;
/// only a registered one becomes a series.
///
/// # Arguments
/// * `name` - the tool name from the request.
///
/// # Returns
/// A name from `mcp::annotations`'s lists, or `"other"`.
#[tracing::instrument(level = "trace", skip_all)]
pub fn tool_label(name: &str) -> &'static str {
    READS
        .iter()
        .chain(STAGED)
        .chain(DESTRUCTIVE)
        .find(|known| **known == name)
        .copied()
        .unwrap_or("other")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_label_passes_registered_names_and_folds_the_rest() {
        assert_eq!(tool_label("get_concept"), "get_concept");
        assert_eq!(tool_label("publish"), "publish");
        assert_eq!(tool_label("open_changeset"), "open_changeset");
        assert_eq!(tool_label("not_a_tool"), "other");
        assert_eq!(tool_label(""), "other");
        assert_eq!(tool_label("get_concept "), "other");
    }

    #[test]
    fn outcome_labels_are_fixed() {
        assert_eq!(Outcome::Ok.as_str(), "ok");
        assert_eq!(Outcome::Error.as_str(), "error");
    }

    #[test]
    fn the_task_5_labels_are_fixed_snake_case_words() {
        let limiters = [
            (Limiter::SignInPerIp, "sign_in_per_ip"),
            (Limiter::ForgotPerEmail, "forgot_per_email"),
            (Limiter::ForgotPerIp, "forgot_per_ip"),
            (Limiter::LinkStarts, "link_starts"),
            (Limiter::OauthRegister, "oauth_register"),
            (Limiter::OauthToken, "oauth_token"),
            (Limiter::OauthConsent, "oauth_consent"),
        ];
        for (limiter, label) in limiters {
            assert_eq!(limiter.as_str(), label);
        }
        assert_eq!(Grant::AuthorizationCode.as_str(), "authorization_code");
        assert_eq!(Grant::RefreshToken.as_str(), "refresh_token");
        assert_eq!(MailOutcome::Sent.as_str(), "sent");
        assert_eq!(MailOutcome::Retry.as_str(), "retry");
        assert_eq!(MailOutcome::Failed.as_str(), "failed");
    }

    #[test]
    fn a_mail_status_parses_only_its_own_spelling() {
        for status in MailStatus::ALL {
            assert_eq!(MailStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(MailStatus::parse("done"), None);
        assert_eq!(MailStatus::parse("Pending"), None);
    }

    #[test]
    fn the_buckets_rise_and_span_a_millisecond_to_a_minute() {
        assert!(SECONDS_BUCKETS.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(SECONDS_BUCKETS.first(), Some(&0.001));
        assert_eq!(SECONDS_BUCKETS.last(), Some(&60.0));
    }
}

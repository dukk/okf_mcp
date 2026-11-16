//! OTLP/HTTP targets and exporters: where telemetry is sent, with which
//! headers, and a counting wrapper that records every failed export.
//!
//! The exporters send with the async `reqwest` client the rest of the crate
//! uses, with redirects off (a redirect would forward the credential headers
//! to wherever the receiver points).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use opentelemetry_otlp::{Protocol, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::logs::{LogBatch, LogExporter};
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::metrics::Temporality;
use opentelemetry_sdk::trace::{SpanData, SpanExporter};
use opentelemetry_sdk::Resource;

use super::config::OtlpConfig;
use crate::error::OkfError;

/// How long one export request may take (the client's and the exporter's
/// timeout).
pub const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);

/// The `[telemetry.otlp]` problems found at start-up are reported against
/// this path.
const CONFIG_PATH: &str = "[telemetry.otlp]";

/// The three OTLP signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Spans.
    Traces,
    /// Log records.
    Logs,
    /// Metrics.
    Metrics,
}

impl Signal {
    /// Every signal, in a fixed order.
    pub const ALL: [Signal; 3] = [Signal::Traces, Signal::Logs, Signal::Metrics];

    /// The URL path segment OTLP/HTTP uses for this signal.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    pub fn path(self) -> &'static str {
        match self {
            Signal::Traces => "traces",
            Signal::Logs => "logs",
            Signal::Metrics => "metrics",
        }
    }

    /// The closed label value for this signal.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    pub fn label(self) -> &'static str {
        self.path()
    }

    /// This signal's position in [`Signal::ALL`].
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    fn index(self) -> usize {
        match self {
            Signal::Traces => 0,
            Signal::Logs => 1,
            Signal::Metrics => 2,
        }
    }
}

/// Why an export failed (a closed set, safe as a metric label).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    /// The export did not finish in time.
    Timeout,
    /// The request never got a response (connection or network error).
    Transport,
    /// The receiver answered with an error status.
    Rejected,
    /// Anything else.
    Other,
}

impl FailureReason {
    /// Every reason, in a fixed order.
    pub const ALL: [FailureReason; 4] = [
        FailureReason::Timeout,
        FailureReason::Transport,
        FailureReason::Rejected,
        FailureReason::Other,
    ];

    /// The closed label value for this reason.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    pub fn label(self) -> &'static str {
        match self {
            FailureReason::Timeout => "timeout",
            FailureReason::Transport => "transport",
            FailureReason::Rejected => "rejected",
            FailureReason::Other => "other",
        }
    }

    /// This reason's position in [`FailureReason::ALL`].
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    fn index(self) -> usize {
        match self {
            FailureReason::Timeout => 0,
            FailureReason::Transport => 1,
            FailureReason::Rejected => 2,
            FailureReason::Other => 3,
        }
    }

    /// Classifies an SDK error.
    ///
    /// The SDK reports an HTTP failure as `InternalFailure` with a message
    /// starting `HTTP export failed`; a message that also mentions `network`
    /// or `connect` never reached the receiver, any other one was answered
    /// with a status.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    pub fn classify(error: &OTelSdkError) -> Self {
        match error {
            OTelSdkError::Timeout(_) => FailureReason::Timeout,
            OTelSdkError::InternalFailure(message) if message.contains("HTTP export failed") => {
                if message.contains("network") || message.contains("connect") {
                    FailureReason::Transport
                } else {
                    FailureReason::Rejected
                }
            }
            OTelSdkError::InternalFailure(_) | OTelSdkError::AlreadyShutdown => {
                FailureReason::Other
            }
        }
    }
}

/// Failed-export counts, one per `(signal, reason)`, shared by every
/// counting exporter. Task 2 renders them on `/metrics`.
#[derive(Debug, Default)]
pub struct ExportFailures {
    counts: [[AtomicU64; 4]; 3],
}

impl ExportFailures {
    /// Counts one failed export.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    pub fn record(&self, signal: Signal, reason: FailureReason) {
        self.counts[signal.index()][reason.index()].fetch_add(1, Ordering::Relaxed);
    }

    /// How many exports of `signal` failed for `reason`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    pub fn count(&self, signal: Signal, reason: FailureReason) -> u64 {
        self.counts[signal.index()][reason.index()].load(Ordering::Relaxed)
    }

    /// How many exports of `signal` failed for any reason.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    pub fn total(&self, signal: Signal) -> u64 {
        FailureReason::ALL
            .iter()
            .map(|reason| self.count(signal, *reason))
            .sum()
    }

    /// Counts the failure in `result`, if any, and returns it unchanged.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    fn observe(&self, signal: Signal, result: OTelSdkResult) -> OTelSdkResult {
        if let Err(error) = &result {
            self.record(signal, FailureReason::classify(error));
        }
        result
    }
}

/// Parses the `name=value,name=value` header format (the variable
/// `headers_env` names); `none` means no headers.
///
/// Names are lower-cased (HTTP names are case-insensitive) and checked as
/// header names; values are trimmed and checked as header values. A later
/// entry for the same name replaces an earlier one.
///
/// # Arguments
/// * `text` - the variable's value.
///
/// # Returns
/// Header name to value.
///
/// # Errors
/// A message that names the entry's position and never its text: the values
/// are credentials.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
    )
)]
pub fn parse_headers(text: &str) -> Result<HashMap<String, String>, String> {
    let text = text.trim();
    let mut headers = HashMap::new();
    if text.eq_ignore_ascii_case("none") {
        return Ok(headers);
    }
    for (position, entry) in text
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .enumerate()
    {
        let position = position + 1;
        let (name, value) = entry
            .split_once('=')
            .ok_or_else(|| format!("header entry {position} has no '='"))?;
        let name = reqwest::header::HeaderName::from_bytes(name.trim().as_bytes())
            .map_err(|_| format!("header entry {position} has an invalid name"))?;
        let value = value.trim();
        reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| format!("header entry {position} has an invalid value"))?;
        headers.insert(name.as_str().to_string(), value.to_string());
    }
    Ok(headers)
}

/// Where OTLP goes and with which headers, resolved from the config and the
/// environment at start-up.
// A hand-written `Debug` (plan Decision 8): header values carry credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct OtlpTarget {
    /// The base URL, without a trailing slash.
    base: String,
    /// Header name to value.
    headers: HashMap<String, String>,
}

impl std::fmt::Debug for OtlpTarget {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Only the header names; validation already refused a base URL with
        // userinfo, query or fragment.
        let mut names: Vec<&str> = self.headers.keys().map(String::as_str).collect();
        names.sort_unstable();
        f.debug_struct("OtlpTarget")
            .field("base", &self.base)
            .field("headers", &names)
            .finish()
    }
}

/// A `[telemetry.otlp]` problem found at start-up.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
    )
)]
fn config_error(message: String) -> OkfError {
    OkfError::Config {
        path: CONFIG_PATH.to_string(),
        message,
    }
}

/// Reads the variable a config key names; unset or blank is an error that
/// names the variable and never a value.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
    )
)]
fn read_env(
    env: &dyn Fn(&str) -> Option<String>,
    name: &str,
    key: &str,
) -> Result<String, OkfError> {
    match env(name) {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(config_error(format!(
            "{key} names {name}, which is not set or is empty"
        ))),
    }
}

impl OtlpTarget {
    /// Resolves the target `[telemetry.otlp]` names.
    ///
    /// # Arguments
    /// * `config` - the validated table.
    /// * `env` - reads an environment variable (injected for tests).
    ///
    /// # Returns
    /// The base URL and headers.
    ///
    /// # Errors
    /// [`OkfError::Config`] when the variable `headers_env` or
    /// `sentry_dsn_env` names is unset or blank, or its value does not parse.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    pub fn resolve(
        config: &OtlpConfig,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, OkfError> {
        if let Some(name) = &config.sentry_dsn_env {
            let dsn = read_env(env, name, "telemetry.otlp.sentry_dsn_env")?;
            return Self::from_sentry_dsn(&dsn).map_err(|message| {
                config_error(format!("the DSN in {name} is not usable: {message}"))
            });
        }
        let (Some(endpoint), Some(name)) = (&config.endpoint, &config.headers_env) else {
            return Err(config_error(
                "needs endpoint and headers_env, or sentry_dsn_env".to_string(),
            ));
        };
        // `headers_env = "none"` is the documented no-auth setting: it names
        // no variable, so the environment is not read at all.
        let headers = if name.trim().eq_ignore_ascii_case("none") {
            HashMap::new()
        } else {
            let text = read_env(env, name, "telemetry.otlp.headers_env")?;
            parse_headers(&text).map_err(|message| {
                config_error(format!("{name} is not name=value,name=value: {message}"))
            })?
        };
        Ok(Self {
            base: endpoint.trim_end_matches('/').to_string(),
            headers,
        })
    }

    /// The OTLP target of a Sentry DSN (`https://<key>@<host>/<project>`).
    ///
    /// # Arguments
    /// * `dsn` - the DSN.
    ///
    /// # Returns
    /// `<scheme>://<host>[:port]<prefix>api/<project>/integration/otlp`, with
    /// the key in an `x-sentry-auth` header.
    ///
    /// # Errors
    /// A message that never contains the key: not a URL, an unusable scheme
    /// or host, an empty key, or a project id that is not digits.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    pub fn from_sentry_dsn(dsn: &str) -> Result<Self, String> {
        let url = url::Url::parse(dsn).map_err(|_| "it is not a URL".to_string())?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err("the scheme must be http or https".to_string());
        }
        let host = url.host_str().ok_or("it has no host")?;
        let key = url.username();
        if key.is_empty() {
            return Err("it has no public key".to_string());
        }
        let path = url.path().trim_end_matches('/');
        let (prefix, project) = path.rsplit_once('/').ok_or("it has no project id")?;
        if project.is_empty() || !project.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("the project id must be digits".to_string());
        }
        let port = url
            .port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default();
        let base = format!(
            "{}://{host}{port}{prefix}/api/{project}/integration/otlp",
            url.scheme()
        );
        let headers = HashMap::from([(
            "x-sentry-auth".to_string(),
            format!("sentry sentry_key={key}"),
        )]);
        Ok(Self { base, headers })
    }

    /// The URL one signal is posted to.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    pub fn signal_url(&self, signal: Signal) -> String {
        format!("{}/v1/{}", self.base, signal.path())
    }

    /// The headers every request carries.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    pub fn headers(&self) -> &HashMap<String, String> {
        &self.headers
    }
}

/// The client every OTLP exporter sends with: no redirects, a bounded
/// request time.
///
/// # Errors
/// [`OkfError::Config`] when the client cannot be built.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
    )
)]
pub(crate) fn http_client() -> Result<reqwest::Client, OkfError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(|error| config_error(format!("OTLP HTTP client: {error}")))
}

/// A span exporter that counts its failures.
///
/// `E` is a generic parameter (a type chosen by the caller, like a Python
/// `TypeVar`): the real OTLP exporter in production, an in-memory or hanging
/// one in tests.
#[derive(Debug)]
pub struct CountingSpanExporter<E> {
    inner: E,
    failures: Arc<ExportFailures>,
}

impl<E> CountingSpanExporter<E> {
    /// Wraps `inner`, counting its failures in `failures`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    pub fn new(inner: E, failures: Arc<ExportFailures>) -> Self {
        Self { inner, failures }
    }
}

impl<E: SpanExporter> SpanExporter for CountingSpanExporter<E> {
    // `async fn` in a trait impl: the trait declares `-> impl Future + Send`,
    // which an `async fn` whose body is `Send` satisfies.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        let result = self.inner.export(batch).await;
        self.failures.observe(Signal::Traces, result)
    }

    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    fn set_resource(&mut self, resource: &Resource) {
        self.inner.set_resource(resource);
    }
}

/// A log exporter that counts its failures.
#[derive(Debug)]
pub struct CountingLogExporter<E> {
    inner: E,
    failures: Arc<ExportFailures>,
}

impl<E> CountingLogExporter<E> {
    /// Wraps `inner`, counting its failures in `failures`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    pub fn new(inner: E, failures: Arc<ExportFailures>) -> Self {
        Self { inner, failures }
    }
}

impl<E: LogExporter> LogExporter for CountingLogExporter<E> {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    async fn export(&self, batch: LogBatch<'_>) -> OTelSdkResult {
        let result = self.inner.export(batch).await;
        self.failures.observe(Signal::Logs, result)
    }

    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    fn event_enabled(
        &self,
        level: opentelemetry::logs::Severity,
        target: &str,
        name: Option<&str>,
    ) -> bool {
        self.inner.event_enabled(level, target, name)
    }

    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
        )
    )]
    fn set_resource(&mut self, resource: &Resource) {
        self.inner.set_resource(resource);
    }
}

/// A metric exporter that counts its failures.
#[derive(Debug)]
pub struct CountingMetricExporter<E> {
    inner: E,
    failures: Arc<ExportFailures>,
}

impl<E> CountingMetricExporter<E> {
    /// Wraps `inner`, counting its failures in `failures`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line constructor")
    )]
    pub fn new(inner: E, failures: Arc<ExportFailures>) -> Self {
        Self { inner, failures }
    }
}

impl<E: PushMetricExporter> PushMetricExporter for CountingMetricExporter<E> {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "delegates to the wrapped exporter and counts its result"
        )
    )]
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        let result = self.inner.export(metrics).await;
        self.failures.observe(Signal::Metrics, result)
    }

    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "constant trait plumbing; counts only what `export` returns"
        )
    )]
    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "constant trait plumbing; counts only what `export` returns"
        )
    )]
    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "constant trait plumbing; counts only what `export` returns"
        )
    )]
    fn temporality(&self) -> Temporality {
        self.inner.temporality()
    }
}

/// Builds the OTLP/HTTP span exporter for `target`.
///
/// # Errors
/// [`OkfError::Config`] when the HTTP client or the exporter cannot be built.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
    )
)]
pub(crate) fn span_exporter(
    target: &OtlpTarget,
    failures: &Arc<ExportFailures>,
) -> Result<CountingSpanExporter<opentelemetry_otlp::SpanExporter>, OkfError> {
    // `use ... {WithExportConfig, WithHttpConfig}` above brings the builder
    // methods into scope (a Rust trait's methods are callable only where the
    // trait is imported, unlike Python's duck typing).
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(target.signal_url(Signal::Traces))
        .with_headers(target.headers.clone())
        .with_http_client(http_client()?)
        .with_timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(|error| config_error(format!("OTLP span exporter: {error}")))?;
    Ok(CountingSpanExporter::new(exporter, Arc::clone(failures)))
}

/// Builds the OTLP/HTTP log exporter for `target`.
///
/// # Errors
/// [`OkfError::Config`] when the HTTP client or the exporter cannot be built.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "exporter or its start-up helper: a span here feeds back into the exporter, or runs before the subscriber exists"
    )
)]
pub(crate) fn log_exporter(
    target: &OtlpTarget,
    failures: &Arc<ExportFailures>,
) -> Result<CountingLogExporter<opentelemetry_otlp::LogExporter>, OkfError> {
    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(target.signal_url(Signal::Logs))
        .with_headers(target.headers.clone())
        .with_http_client(http_client()?)
        .with_timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(|error| config_error(format!("OTLP log exporter: {error}")))?;
    Ok(CountingLogExporter::new(exporter, Arc::clone(failures)))
}

/// Builds the OTLP/HTTP metric exporter for `target`, cumulative (the same
/// temporality the Prometheus reader has).
///
/// # Errors
/// [`OkfError::Config`] when the HTTP client or the exporter cannot be built.
#[tracing::instrument(level = "trace", skip_all)]
pub(crate) fn metric_exporter(
    target: &OtlpTarget,
    failures: &Arc<ExportFailures>,
) -> Result<CountingMetricExporter<opentelemetry_otlp::MetricExporter>, OkfError> {
    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(target.signal_url(Signal::Metrics))
        .with_headers(target.headers.clone())
        .with_http_client(http_client()?)
        .with_timeout(EXPORT_TIMEOUT)
        .with_temporality(Temporality::Cumulative)
        .build()
        .map_err(|error| config_error(format!("OTLP metric exporter: {error}")))?;
    Ok(CountingMetricExporter::new(exporter, Arc::clone(failures)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn sentry_dsn_gives_the_otlp_endpoint_and_auth_header() {
        let target =
            OtlpTarget::from_sentry_dsn("https://public@sentry.example.com/42").expect("parses");
        assert_eq!(
            target.signal_url(Signal::Traces),
            "https://sentry.example.com/api/42/integration/otlp/v1/traces"
        );
        assert_eq!(
            target.headers().get("x-sentry-auth").map(String::as_str),
            Some("sentry sentry_key=public")
        );
    }

    #[test]
    fn sentry_dsn_keeps_a_path_prefix_and_a_port() {
        let target =
            OtlpTarget::from_sentry_dsn("http://public@sentry.example.com:9000/relay/sub/7/")
                .expect("parses");
        assert_eq!(
            target.signal_url(Signal::Logs),
            "http://sentry.example.com:9000/relay/sub/api/7/integration/otlp/v1/logs"
        );
    }

    #[test]
    fn a_malformed_dsn_error_never_contains_the_key() {
        for dsn in [
            "https://hunter2@sentry.example.com/not-digits",
            "https://hunter2@sentry.example.com/",
            "ftp://hunter2@sentry.example.com/42",
            "hunter2@sentry.example.com/42",
            "https://sentry.example.com/42",
        ] {
            let error = OtlpTarget::from_sentry_dsn(dsn).expect_err(dsn);
            assert!(!error.contains("hunter2"), "{dsn}: {error}");
        }
    }

    #[test]
    fn resolve_reads_headers_through_the_injected_environment() {
        let config = OtlpConfig {
            endpoint: Some("https://otlp.example.com/".to_string()),
            headers_env: Some("OTLP_HEADERS".to_string()),
            ..OtlpConfig::default()
        };
        let target = OtlpTarget::resolve(&config, &|name| {
            (name == "OTLP_HEADERS").then(|| "Authorization=Bearer abc, x-team=core".to_string())
        })
        .expect("resolves");
        assert_eq!(
            target.signal_url(Signal::Metrics),
            "https://otlp.example.com/v1/metrics"
        );
        assert_eq!(target.headers().len(), 2);
        assert_eq!(target.headers()["authorization"], "Bearer abc");
    }

    #[test]
    fn headers_env_none_sends_no_headers_and_reads_no_variable() {
        use crate::config::Config;

        // From the config file to the target, with no variable set at all.
        let config = Config::from_toml(
            "[telemetry.otlp]\nendpoint = \"https://otlp.example.com\"\nheaders_env = \"none\"\n",
        )
        .expect("parses");
        config.validate().expect("valid");
        let otlp = config.telemetry.otlp.expect("otlp table");
        let target = OtlpTarget::resolve(&otlp, &|name| panic!("read {name}")).expect("resolves");
        assert!(target.headers().is_empty());
        assert_eq!(
            target.signal_url(Signal::Traces),
            "https://otlp.example.com/v1/traces"
        );
        // The sentinel is case-insensitive, as in `parse_headers`.
        let upper = OtlpConfig {
            headers_env: Some("None".to_string()),
            ..otlp
        };
        assert!(OtlpTarget::resolve(&upper, &no_env)
            .expect("resolves")
            .headers()
            .is_empty());
    }

    #[test]
    fn the_sentry_dsn_path_has_no_sentinel_and_needs_its_variable() {
        // `sentry_dsn_env` is always a variable name: a DSN is required.
        let config = OtlpConfig {
            sentry_dsn_env: Some("none".to_string()),
            ..OtlpConfig::default()
        };
        let error = OtlpTarget::resolve(&config, &no_env).expect_err("needs a DSN");
        assert!(error.to_string().contains("none"), "{error}");
    }

    #[test]
    fn resolve_names_the_variable_and_never_a_value() {
        let config = OtlpConfig {
            endpoint: Some("https://otlp.example.com".to_string()),
            headers_env: Some("OTLP_HEADERS".to_string()),
            ..OtlpConfig::default()
        };
        let unset = OtlpTarget::resolve(&config, &no_env).expect_err("unset");
        assert!(unset.to_string().contains("OTLP_HEADERS"), "{unset}");
        let blank = OtlpTarget::resolve(&config, &|_| Some("  ".to_string())).expect_err("blank");
        assert!(blank.to_string().contains("OTLP_HEADERS"), "{blank}");
        let bad = OtlpTarget::resolve(&config, &|_| Some("token-value-no-equals".to_string()))
            .expect_err("bad");
        assert!(bad.to_string().contains("OTLP_HEADERS"), "{bad}");
        assert!(!bad.to_string().contains("token-value"), "{bad}");
    }

    #[test]
    fn debug_prints_header_names_only() {
        let target =
            OtlpTarget::from_sentry_dsn("https://hunter2@sentry.example.com/42").expect("parses");
        let text = format!("{target:?}");
        assert!(text.contains("x-sentry-auth"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
    }

    #[test]
    fn none_means_no_headers() {
        assert!(parse_headers("none").expect("parses").is_empty());
        assert!(parse_headers(" NONE ").expect("parses").is_empty());
    }

    #[test]
    fn a_pair_without_equals_is_refused_without_its_text() {
        let error = parse_headers("a=1,secret-token").expect_err("refused");
        assert!(error.contains("entry 2"), "{error}");
        assert!(!error.contains("secret-token"), "{error}");
        let error = parse_headers("bad name=secret-value").expect_err("refused");
        assert!(!error.contains("secret-value"), "{error}");
        let error = parse_headers("a=secret\nvalue").expect_err("refused");
        assert!(!error.contains("secret"), "{error}");
    }

    #[hegel::test]
    fn headers_round_trip(tc: hegel::TestCase) {
        use hegel::generators;
        let names: Vec<String> = tc.draw(
            generators::vecs(generators::from_regex("[a-z][a-z0-9-]{0,11}"))
                .unique(true)
                .max_size(6),
        );
        let values: Vec<String> = tc.draw(
            generators::vecs(generators::from_regex("[A-Za-z0-9._~/+=:;@ -]{1,20}"))
                .min_size(names.len())
                .max_size(names.len()),
        );
        let expected: HashMap<String, String> = names
            .iter()
            .cloned()
            .zip(values.iter().map(|value| value.trim().to_string()))
            .collect();
        let text = names
            .iter()
            .zip(&values)
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(parse_headers(&text).expect("parses"), expected);
    }

    #[test]
    fn every_sdk_error_has_its_reason() {
        let cases = [
            (
                OTelSdkError::Timeout(Duration::from_secs(1)),
                FailureReason::Timeout,
            ),
            (
                OTelSdkError::InternalFailure("HTTP export failed: network error".to_string()),
                FailureReason::Transport,
            ),
            (
                OTelSdkError::InternalFailure("HTTP export failed: connect refused".to_string()),
                FailureReason::Transport,
            ),
            (
                OTelSdkError::InternalFailure(
                    "HTTP export failed with status code: 503".to_string(),
                ),
                FailureReason::Rejected,
            ),
            (
                OTelSdkError::InternalFailure("lock poisoned".to_string()),
                FailureReason::Other,
            ),
            (OTelSdkError::AlreadyShutdown, FailureReason::Other),
        ];
        for (error, reason) in cases {
            assert_eq!(FailureReason::classify(&error), reason, "{error}");
        }
    }

    #[test]
    fn failures_are_counted_per_signal_and_reason() {
        let failures = ExportFailures::default();
        failures.record(Signal::Traces, FailureReason::Timeout);
        failures.record(Signal::Traces, FailureReason::Timeout);
        failures.record(Signal::Logs, FailureReason::Rejected);
        assert_eq!(failures.count(Signal::Traces, FailureReason::Timeout), 2);
        assert_eq!(failures.count(Signal::Logs, FailureReason::Timeout), 0);
        assert_eq!(failures.total(Signal::Logs), 1);
        assert_eq!(failures.total(Signal::Metrics), 0);
    }

    /// A span exporter whose export always fails.
    #[derive(Debug)]
    struct Failing;

    impl SpanExporter for Failing {
        async fn export(&self, _batch: Vec<SpanData>) -> OTelSdkResult {
            Err(OTelSdkError::Timeout(Duration::from_secs(1)))
        }
    }

    #[tokio::test]
    async fn the_counting_exporter_counts_a_failed_export_and_passes_it_on() {
        let failures = Arc::new(ExportFailures::default());
        let exporter = CountingSpanExporter::new(Failing, Arc::clone(&failures));
        let result = exporter.export(Vec::new()).await;
        assert!(matches!(result, Err(OTelSdkError::Timeout(_))));
        assert_eq!(failures.count(Signal::Traces, FailureReason::Timeout), 1);
    }

    /// A metric exporter whose export always fails.
    #[derive(Debug)]
    struct FailingMetrics;

    impl PushMetricExporter for FailingMetrics {
        async fn export(&self, _metrics: &ResourceMetrics) -> OTelSdkResult {
            Err(OTelSdkError::Timeout(Duration::from_secs(1)))
        }

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            Ok(())
        }

        fn temporality(&self) -> Temporality {
            Temporality::Cumulative
        }
    }

    #[tokio::test]
    async fn the_counting_metric_exporter_counts_a_failed_export() {
        let failures = Arc::new(ExportFailures::default());
        let exporter = CountingMetricExporter::new(FailingMetrics, Arc::clone(&failures));
        let result = exporter.export(&ResourceMetrics::default()).await;
        assert!(matches!(result, Err(OTelSdkError::Timeout(_))));
        assert_eq!(failures.count(Signal::Metrics, FailureReason::Timeout), 1);
    }
}

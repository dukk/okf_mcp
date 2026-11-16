//! The server's metric instruments, behind one typed handle.
//!
//! Callers record through [`Metrics`]'s methods only: never an instrument,
//! never a raw attribute. Every label value is a closed set (an enum's
//! `as_str`, or [`tool_label`]'s allow-list), so a series count cannot grow
//! with what a client sends (spec "The `Metrics` handle").

use std::sync::Arc;
use std::time::Duration;

use opentelemetry::metrics::{Histogram, Meter, UpDownCounter};
use opentelemetry::KeyValue;

use super::otlp::{ExportFailures, FailureReason, Signal};
use crate::mcp::annotations::{DESTRUCTIVE, READS, STAGED};

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
}

/// Every instrument, built once from a [`Meter`].
#[derive(Debug)]
struct Instruments {
    // Task 3: HTTP and MCP.
    http_duration: Histogram<f64>,
    http_active: UpDownCounter<i64>,
    tool_duration: Histogram<f64>,
    mcp_sessions: UpDownCounter<i64>,
}

impl Metrics {
    /// The Prometheus family names that exist before any traffic: the
    /// observable ones, which report from the first scrape. [`Metrics::new`]
    /// and [`observe_export_failures`] together make them present. Tasks 4-6
    /// add the catalog, changeset, mail-queue, pool and clock gauges.
    pub const FAMILIES: &'static [&'static str] =
        &["okf_build_info", "okf_telemetry_export_failures_total"];

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
            }),
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
    pub fn tool_call(&self, tool: &'static str, catalog: &str, outcome: Outcome, elapsed: Duration) {
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
    fn the_buckets_rise_and_span_a_millisecond_to_a_minute() {
        assert!(SECONDS_BUCKETS.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(SECONDS_BUCKETS.first(), Some(&0.001));
        assert_eq!(SECONDS_BUCKETS.last(), Some(&60.0));
    }
}

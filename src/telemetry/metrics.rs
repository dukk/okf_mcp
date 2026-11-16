//! The server's metric instruments, behind one typed handle.
//!
//! Callers record through [`Metrics`]'s methods only: never an instrument,
//! never a raw attribute. Every label value is a closed set (an enum's
//! `as_str`, or [`tool_label`]'s allow-list), so a series count cannot grow
//! with what a client sends (spec "The `Metrics` handle").

use std::sync::Arc;

use opentelemetry::metrics::{Histogram, Meter};
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
    // methods that read them; until then nothing reads it.
    #[allow(
        dead_code,
        reason = "the instruments Tasks 3-6 add are read through it"
    )]
    inner: Arc<Instruments>,
}

/// Every instrument, built once from a [`Meter`].
#[derive(Debug)]
struct Instruments {}

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
            inner: Arc::new(Instruments {}),
        }
    }

    /// The instruments on the global meter provider: a no-op until `serve`
    /// sets one, so tests and the other subcommands record nothing.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn global() -> Self {
        Self::new(&opentelemetry::global::meter("okf-mcp"))
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

//! The server's metric instruments, behind one typed handle.
//!
//! Callers record through [`Metrics`]'s methods only: never an instrument,
//! never a raw attribute. Every label value is a closed set (an enum's
//! `as_str`, or [`tool_label`]'s allow-list), so a series count cannot grow
//! with what a client sends (spec "The `Metrics` handle").

use std::sync::{Arc, Weak};
use std::time::Duration;

use opentelemetry::metrics::{Counter, Histogram, Meter};
use opentelemetry::KeyValue;

use super::otlp::{ExportFailures, FailureReason, Signal};
use crate::app::App;
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

/// What one catalog's turn in a poll pass came to: a closed set for the
/// `result` label of `okf.catalog.refreshes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshResult {
    /// Nothing to do: the snapshot is current, or a failed generation waits.
    Unchanged,
    /// The loaded catalog's snapshot was rebuilt in place.
    Reloaded,
    /// The catalog was loaded from scratch (new, or created again).
    Loaded,
    /// The refresh or load failed.
    Failed,
}

impl RefreshResult {
    /// The label value (also the `okf.refresh.result` span attribute).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Reloaded => "reloaded",
            Self::Loaded => "loaded",
            Self::Failed => "failed",
        }
    }
}

/// The `rule` label for a lint finding's rule id: the id itself when it is
/// one of the lint's own fixed rules, `secret` for every `secret:<id>` rule
/// (the token table is the lint's, and a label need not name which pattern
/// matched), and `other` for anything else. A label never carries text.
///
/// # Arguments
/// * `rule` - a finding's `rule`.
///
/// # Returns
/// A name from a fixed list.
#[tracing::instrument(level = "trace", skip_all)]
pub fn lint_rule_label(rule: &str) -> &'static str {
    if rule.starts_with("secret:") {
        return "secret";
    }
    LINT_RULES
        .iter()
        .find(|known| **known == rule)
        .copied()
        .unwrap_or("other")
}

/// The lint's own rule ids (`okf::analysis::lint`, plus publish's
/// `link-into-deleted`).
const LINT_RULES: &[&str] = &[
    "frontmatter-parse",
    "index-frontmatter",
    "no-frontmatter",
    "no-type",
    "okf-version",
    "sprawl",
    "status",
    "generated-after-commit",
    "generated-shape",
    "verified-shape",
    "reviewed-shape",
    "reviewed-outcome",
    "stale",
    "timestamp",
    "timestamp-utc",
    "future-timestamp",
    "broken-link",
    "broken-anchor",
    "relative-link",
    "orphan",
    "index-drift",
    "link-into-deleted",
];

/// The server's instruments.
///
/// Cheap to clone: the instruments sit behind an `Arc`, so [`crate::app::App`]
/// and any callback share one set.
#[derive(Debug, Clone)]
pub struct Metrics {
    // Tasks 3, 5 and 6 add one field per instrument to `Instruments`, and the
    // typed methods that read them.
    inner: Arc<Instruments>,
    // Observable gauges are registered after the `App` exists, on this meter.
    meter: Meter,
}

/// Every instrument, built once from a [`Meter`].
#[derive(Debug)]
struct Instruments {
    publish_duration: Histogram<f64>,
    publish_lock_wait: Histogram<f64>,
    lint_rejections: Counter<u64>,
    poll_duration: Histogram<f64>,
    catalog_refreshes: Counter<u64>,
    keycache_reloads: Counter<u64>,
}

impl Metrics {
    /// The Prometheus family names that exist before any traffic: the
    /// observable ones, which report from the first scrape. [`Metrics::new`]
    /// and [`observe_export_failures`] together make them present. Tasks 4-6
    /// add the catalog, changeset, mail-queue, pool and clock gauges.
    pub const FAMILIES: &'static [&'static str] =
        &["okf_build_info", "okf_telemetry_export_failures_total"];
    // The catalog gauges join them once `App::load_with_metrics` registers
    // them (`observe_catalogs`); they have a series only per loaded catalog.

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
                publish_duration: seconds_histogram(
                    meter,
                    "okf.publish.duration",
                    "Publishes, from the first lock to the refreshed snapshot.",
                ),
                publish_lock_wait: seconds_histogram(
                    meter,
                    "okf.publish.lock_wait",
                    "Time a publish waited for the catalog's locks.",
                ),
                lint_rejections: meter
                    .u64_counter("okf.lint.rejections")
                    .with_description("Publish lint errors, by rule.")
                    .build(),
                poll_duration: seconds_histogram(
                    meter,
                    "okf.poll.duration",
                    "Poll passes, by outcome.",
                ),
                catalog_refreshes: meter
                    .u64_counter("okf.catalog.refreshes")
                    .with_description("Each catalog's turn in a poll pass, by result.")
                    .build(),
                keycache_reloads: meter
                    .u64_counter("okf.keycache.reloads")
                    .with_description("Key cache and settings reloads, by outcome.")
                    .build(),
            }),
            meter: meter.clone(),
        }
    }

    /// Records a finished publish.
    ///
    /// # Arguments
    /// * `catalog` - the catalog's name (a validated catalog name).
    /// * `outcome` - whether it committed (or had nothing to write).
    /// * `elapsed` - the whole publish.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn publish(&self, catalog: &str, outcome: Outcome, elapsed: Duration) {
        self.inner.publish_duration.record(
            elapsed.as_secs_f64(),
            &[
                KeyValue::new("catalog", catalog.to_owned()),
                KeyValue::new("outcome", outcome.as_str()),
            ],
        );
    }

    /// Records how long a publish waited for the catalog's locks.
    ///
    /// # Arguments
    /// * `catalog` - the catalog's name.
    /// * `waited` - the time from asking to holding every lock.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn publish_lock_wait(&self, catalog: &str, waited: Duration) {
        self.inner.publish_lock_wait.record(
            waited.as_secs_f64(),
            &[KeyValue::new("catalog", catalog.to_owned())],
        );
    }

    /// Counts one lint error that refused a publish.
    ///
    /// # Arguments
    /// * `catalog` - the catalog's name.
    /// * `rule` - the finding's rule id; folded by [`lint_rule_label`].
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn lint_rejection(&self, catalog: &str, rule: &str) {
        self.inner.lint_rejections.add(
            1,
            &[
                KeyValue::new("catalog", catalog.to_owned()),
                KeyValue::new("rule", lint_rule_label(rule)),
            ],
        );
    }

    /// Records a poll pass.
    ///
    /// # Arguments
    /// * `outcome` - whether the pass returned `Ok`.
    /// * `elapsed` - its duration, the wait for an overlapping pass included.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn poll(&self, outcome: Outcome, elapsed: Duration) {
        self.inner.poll_duration.record(
            elapsed.as_secs_f64(),
            &[KeyValue::new("outcome", outcome.as_str())],
        );
    }

    /// Counts one catalog's turn in a poll pass.
    ///
    /// # Arguments
    /// * `catalog` - the catalog's name.
    /// * `result` - what the turn came to.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn catalog_refresh(&self, catalog: &str, result: RefreshResult) {
        self.inner.catalog_refreshes.add(
            1,
            &[
                KeyValue::new("catalog", catalog.to_owned()),
                KeyValue::new("result", result.as_str()),
            ],
        );
    }

    /// Counts one key cache and settings reload.
    ///
    /// # Arguments
    /// * `outcome` - whether it succeeded.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn keycache_reload(&self, outcome: Outcome) {
        self.inner
            .keycache_reloads
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
    }

    /// Registers the catalog gauges: `okf.catalog.concepts`,
    /// `okf.catalog.generation` and `okf.changesets.open`, one series per
    /// loaded catalog. Each callback holds only a [`Weak`] handle, so the
    /// meter provider (which outlives the app) keeps no strong cycle, and
    /// reads what `app` already holds: a scrape never queries Postgres.
    ///
    /// # Arguments
    /// * `app` - the app to read; a callback finding it dropped reports
    ///   nothing.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn observe_catalogs(&self, app: Weak<App>) {
        // The gauge handles may be dropped: a callback stays registered with
        // the provider (see `observe_export_failures`).
        let concepts = app.clone();
        self.meter
            .u64_observable_gauge("okf.catalog.concepts")
            .with_description("Concept documents in each catalog's current snapshot.")
            .with_callback(move |observer| {
                let Some(app) = concepts.upgrade() else {
                    return;
                };
                for (name, state) in app.catalogs().iter() {
                    let count = state.current().snapshot().concepts().count();
                    observer.observe(
                        u64::try_from(count).unwrap_or(u64::MAX),
                        &[KeyValue::new("catalog", name.clone())],
                    );
                }
            })
            .build();
        let generations = app.clone();
        self.meter
            .i64_observable_gauge("okf.catalog.generation")
            .with_description("Each catalog's current snapshot generation.")
            .with_callback(move |observer| {
                let Some(app) = generations.upgrade() else {
                    return;
                };
                for (name, state) in app.catalogs().iter() {
                    observer.observe(
                        state.current().generation(),
                        &[KeyValue::new("catalog", name.clone())],
                    );
                }
            })
            .build();
        self.meter
            .i64_observable_gauge("okf.changesets.open")
            .with_description("Open changesets per catalog, as of the last poll pass.")
            .with_callback(move |observer| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                for name in app.catalogs().keys() {
                    observer.observe(
                        app.open_changesets(name),
                        &[KeyValue::new("catalog", name.clone())],
                    );
                }
            })
            .build();
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

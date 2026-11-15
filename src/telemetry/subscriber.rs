//! The subscriber: one stderr layer (text, or JSON with trace ids), and the
//! OTLP span and log layers when their providers exist.
//!
//! Nothing filters the whole stack. Each layer has its own filter, so a
//! quiet stderr never silences the exporters (spec Decision 3). Each filter
//! is ANDed with a fixed cap (`cap`) that the configuration cannot lift.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::Level;
use tracing_subscriber::filter::{EnvFilter, FilterExt, LevelFilter, Targets};
use tracing_subscriber::fmt;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{Layer, Registry};

use super::config::{
    parse_filter, LogFormat, TelemetryConfig, DEFAULT_OTLP_FILTER, DEFAULT_STDERR_FILTER,
};
use super::json::{CaptureDispatch, DispatchSlot, JsonWithTrace};

/// Targets the OTLP layers never take: the export stack's own events
/// (`internal-logs` uses each crate's package name as target, some with
/// hyphens) and the HTTP stack it sends with. An export failure exported
/// over the export that failed would loop.
///
/// The OTLP cap is an allow-list of this crate's targets, so none of these
/// needs turning off by name. The list stays as the record of what must
/// never be allow-listed.
pub const EXPORT_STACK: [&str; 10] = [
    "opentelemetry",
    "opentelemetry_sdk",
    "opentelemetry-otlp",
    "opentelemetry-http",
    "opentelemetry-prometheus",
    "opentelemetry-appender-tracing",
    "hyper",
    "hyper_util",
    "reqwest",
    "h2",
];

/// The target of events that may carry text the privacy rules keep out of
/// OTLP: a caller-supplied value, an internal error's full text, the `log`
/// mail provider's message. Such events reach stderr only. `pub` so every
/// module writes `tracing::warn!(target: LOCAL_ONLY, ...)` (Task 1b).
pub const LOCAL_ONLY: &str = "okf_mcp::local_only";

/// The highest level any layer takes from `rmcp`: at `debug` it logs whole
/// JSON-RPC messages, credentials among them.
const RMCP_CAP: Level = Level::INFO;

/// What every layer ANDs with its own filter.
///
/// stderr takes every target, `rmcp` capped. The OTLP layers take **only
/// this crate's own targets** (an allow-list, not a deny-list): dependency
/// events are out of the privacy rules' reach. `sqlx` logs statement text
/// at `debug` (and slow queries at `warn`), and `apalis`, `openidconnect`,
/// the AWS SDK and the export stack ([`EXPORT_STACK`]) have their own.
/// `Targets` picks the most specific matching target, so [`LOCAL_ONLY`] (a
/// sub-target of `okf_mcp`) stays off.
///
/// # Arguments
/// * `otlp` - `true` for the OTLP layers, `false` for stderr.
fn cap(otlp: bool) -> Targets {
    if otlp {
        return Targets::new()
            .with_default(LevelFilter::OFF)
            .with_target("okf_mcp", LevelFilter::TRACE)
            .with_target(LOCAL_ONLY, LevelFilter::OFF);
    }
    Targets::new()
        .with_default(LevelFilter::TRACE)
        .with_target("rmcp", RMCP_CAP)
}

/// The filters and format the layers use, already resolved (`RUST_LOG`
/// applied), so [`build`] is a pure function of its inputs.
#[derive(Debug, Clone)]
pub struct LayerSettings {
    /// stderr's line format.
    pub format: LogFormat,
    /// stderr's filter.
    pub stderr: EnvFilter,
    /// The OTLP span layer's filter.
    pub traces: EnvFilter,
    /// The OTLP log layer's filter.
    pub logs: EnvFilter,
}

/// Resolves the filters for the layers.
///
/// stderr's filter comes from `rust_log`, else `[telemetry.stderr] filter`,
/// else [`DEFAULT_STDERR_FILTER`]; an invalid or blank `RUST_LOG` falls back
/// to the next source, as it always has. The OTLP filters come from
/// `[telemetry.otlp]`, else [`DEFAULT_OTLP_FILTER`]. Directive components are
/// trimmed before parsing.
///
/// # Arguments
/// * `config` - `[telemetry]`, already validated.
/// * `rust_log` - the value of `RUST_LOG`, if set.
///
/// # Returns
/// The settings.
///
/// # Errors
/// A configured filter that does not parse (validation refuses these first).
pub fn settings(config: &TelemetryConfig, rust_log: Option<&str>) -> Result<LayerSettings, String> {
    let from_env = rust_log
        .filter(|text| !text.trim().is_empty())
        .and_then(|text| parse_filter(text).ok());
    let stderr = match from_env {
        Some(filter) => filter,
        None => parse_filter(
            config
                .stderr
                .filter
                .as_deref()
                .unwrap_or(DEFAULT_STDERR_FILTER),
        )
        .map_err(|error| format!("telemetry.stderr.filter: {error}"))?,
    };
    let (traces, logs) = match &config.otlp {
        Some(otlp) => (
            parse_filter(&otlp.traces_filter)
                .map_err(|error| format!("telemetry.otlp.traces_filter: {error}"))?,
            parse_filter(&otlp.logs_filter)
                .map_err(|error| format!("telemetry.otlp.logs_filter: {error}"))?,
        ),
        None => (
            parse_filter(DEFAULT_OTLP_FILTER)?,
            parse_filter(DEFAULT_OTLP_FILTER)?,
        ),
    };
    Ok(LayerSettings {
        format: config.stderr.format,
        stderr,
        traces,
        logs,
    })
}

/// The subscriber: stderr, plus the OTLP span and log layers when their
/// providers exist.
///
/// # Arguments
/// * `settings` - the resolved filters and format.
/// * `tracer` - the span provider, when OTLP traces are on.
/// * `logger` - the log provider, when OTLP logs are on.
///
/// # Returns
/// A subscriber writing to stderr.
pub fn build(
    settings: LayerSettings,
    tracer: Option<&SdkTracerProvider>,
    logger: Option<&SdkLoggerProvider>,
) -> impl tracing::Subscriber + Send + Sync {
    build_with_writer(settings, tracer, logger, std::io::stderr)
}

/// [`build`] with the stderr layer's writer as a parameter, so tests can
/// capture what it writes.
///
/// # Arguments
/// * `settings`, `tracer`, `logger` - as for [`build`].
/// * `writer` - where the text or JSON lines go (`std::io::stderr` in
///   production). A `MakeWriter` is anything that hands out a `Write` per
///   event; a bare function returning a writer is one.
pub fn build_with_writer<W>(
    settings: LayerSettings,
    tracer: Option<&SdkTracerProvider>,
    logger: Option<&SdkLoggerProvider>,
    writer: W,
) -> impl tracing::Subscriber + Send + Sync
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let slot = DispatchSlot::default();
    // A filter's type names the subscriber it is added to, so each arm builds
    // its own. `FilterExt::and` combines two filters: an event passes when both pass.
    // `Option<Layer>` is itself a layer that does nothing when `None`, so the
    // layers that are off need no separate type.
    let (text, json) = match settings.format {
        LogFormat::Text => (
            Some(
                fmt::layer()
                    .with_writer(writer)
                    .with_filter(settings.stderr.and(cap(false))),
            ),
            None,
        ),
        LogFormat::Json => (
            None,
            Some(
                fmt::layer()
                    .with_writer(writer)
                    .event_format(JsonWithTrace(slot.clone()))
                    .with_filter(settings.stderr.and(cap(false))),
            ),
        ),
    };
    let spans = tracer.map(|provider| {
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("okf-mcp"))
            // Error text never becomes an attribute, an event or a status
            // description (privacy rules): an ERROR event still marks the
            // span `Error`, with no description.
            .with_error_fields_to_exceptions(false)
            .with_error_records_to_exceptions(false)
            .with_error_events_to_exceptions(false)
            .with_error_events_to_status(false)
            .with_location(false)
            .with_threads(false)
            .with_target(false)
            .with_tracked_inactivity(false)
            .with_filter(settings.traces.and(cap(true)))
    });
    let logs = logger.map(|provider| {
        OpenTelemetryTracingBridge::new(provider).with_filter(settings.logs.and(cap(true)))
    });
    Registry::default()
        .with(CaptureDispatch(slot))
        .with(text)
        .with(json)
        .with(spans)
        .with(logs)
}

/// The subscriber used while the configuration is being read and telemetry
/// started: stderr text at `RUST_LOG`, else [`DEFAULT_STDERR_FILTER`], no
/// OTLP. It is scoped to start-up only (see [`super::start_up`]).
///
/// # Arguments
/// * `rust_log` - the value of `RUST_LOG`, if set.
/// * `writer` - where the lines go.
pub fn fallback<W>(rust_log: Option<&str>, writer: W) -> impl tracing::Subscriber + Send + Sync
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let filter = rust_log
        .filter(|text| !text.trim().is_empty())
        .and_then(|text| parse_filter(text).ok())
        .or_else(|| parse_filter(DEFAULT_STDERR_FILTER).ok())
        .unwrap_or_else(|| EnvFilter::new("warn"));
    Registry::default().with(
        fmt::layer()
            .with_writer(writer)
            .with_filter(filter.and(cap(false))),
    )
}

/// A capturing stderr for tests: an `Arc<Mutex<Vec<u8>>>` that implements
/// `MakeWriter`.
#[cfg(test)]
pub(crate) mod capture {
    use std::io;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::writer::MakeWriter;

    /// Everything written through it, shared between clones.
    #[derive(Debug, Clone, Default)]
    pub struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        /// What has been written so far, as text.
        pub fn text(&self) -> String {
            let bytes = self.0.lock().expect("capture lock").clone();
            String::from_utf8_lossy(&bytes).into_owned()
        }
    }

    /// One event's writer.
    pub struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CaptureWriter(Arc::clone(&self.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::trace::Status;
    use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLoggerProvider};
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

    use super::capture::Capture;
    use super::*;

    /// Everything one run of a subscriber produced.
    struct Observed {
        stderr: String,
        spans: Vec<opentelemetry_sdk::trace::SpanData>,
        logs: Vec<String>,
    }

    /// Builds the subscriber with in-memory exporters, runs `body` under it
    /// as the thread's default, then shuts the providers down (after the
    /// dispatcher is gone: dropping a provider inside the dispatcher's lock
    /// has deadlocked other projects' tests).
    fn observe(
        stderr: &str,
        traces: &str,
        logs: &str,
        format: LogFormat,
        body: impl FnOnce(),
    ) -> Observed {
        let span_exporter = InMemorySpanExporter::default();
        let log_exporter = InMemoryLogExporter::default();
        let tracer = SdkTracerProvider::builder()
            .with_simple_exporter(span_exporter.clone())
            .build();
        let logger = SdkLoggerProvider::builder()
            .with_simple_exporter(log_exporter.clone())
            .build();
        let capture = Capture::default();
        let settings = LayerSettings {
            format,
            stderr: parse_filter(stderr).expect("stderr filter"),
            traces: parse_filter(traces).expect("traces filter"),
            logs: parse_filter(logs).expect("logs filter"),
        };
        let subscriber = build_with_writer(settings, Some(&tracer), Some(&logger), capture.clone());
        tracing::subscriber::with_default(subscriber, body);
        tracer.force_flush().expect("flush spans");
        logger.force_flush().expect("flush logs");
        let observed = Observed {
            stderr: capture.text(),
            spans: span_exporter.get_finished_spans().expect("spans"),
            logs: log_exporter
                .get_emitted_logs()
                .expect("logs")
                .iter()
                .map(|record| {
                    let body = record.record.body().map(|value| format!("{value:?}"));
                    format!(
                        "{}|{}",
                        record
                            .record
                            .target()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                        body.unwrap_or_default()
                    )
                })
                .collect(),
        };
        let _ = tracer.shutdown();
        let _ = logger.shutdown();
        observed
    }

    #[test]
    fn stderr_at_warn_still_exports_debug_spans() {
        let observed = observe("warn", "debug", "info", LogFormat::Text, || {
            let span = tracing::debug_span!(target: "okf_mcp::quiet", "debug_work");
            let _entered = span.enter();
            tracing::debug!(target: "okf_mcp::quiet", "hidden-debug-line");
        });
        assert!(
            observed.spans.iter().any(|span| span.name == "debug_work"),
            "the debug span reaches the exporter"
        );
        assert!(
            !observed.stderr.contains("hidden-debug-line"),
            "{}",
            observed.stderr
        );
        assert!(
            !observed.stderr.contains("debug_work"),
            "{}",
            observed.stderr
        );
    }

    #[test]
    fn rmcp_debug_never_reaches_any_layer() {
        let observed = observe("trace", "trace", "trace", LogFormat::Text, || {
            let span = tracing::trace_span!(target: "rmcp", "rmcp_span");
            let _entered = span.enter();
            tracing::debug!(target: "rmcp", "rmcp-debug-line");
            tracing::trace!(target: "rmcp", "rmcp-trace-line");
            tracing::info!(target: "rmcp", "rmcp-info-line");
        });
        assert!(
            !observed.stderr.contains("rmcp-debug-line"),
            "{}",
            observed.stderr
        );
        assert!(
            !observed.stderr.contains("rmcp-trace-line"),
            "{}",
            observed.stderr
        );
        // At `info` stderr still shows `rmcp`; the OTLP layers take nothing from it.
        assert!(
            observed.stderr.contains("rmcp-info-line"),
            "{}",
            observed.stderr
        );
        assert!(
            observed.logs.iter().all(|log| !log.contains("rmcp")),
            "{:?}",
            observed.logs
        );
        assert!(observed.spans.is_empty(), "{:?}", observed.spans);
    }

    #[test]
    fn export_stack_events_reach_stderr_only() {
        let observed = observe("trace", "trace", "trace", LogFormat::Text, || {
            tracing::error!(target: "opentelemetry-otlp", "export-failed-line");
            tracing::error!(target: "opentelemetry_sdk", "sdk-failed-line");
            tracing::error!(target: "reqwest", "reqwest-failed-line");
        });
        for line in [
            "export-failed-line",
            "sdk-failed-line",
            "reqwest-failed-line",
        ] {
            assert!(
                observed.stderr.contains(line),
                "{line}: {}",
                observed.stderr
            );
        }
        assert!(observed.logs.is_empty(), "{:?}", observed.logs);
    }

    /// `LOCAL_ONLY` is a sub-target of `okf_mcp`; the cap's most-specific
    /// match must still turn it off.
    #[test]
    fn local_only_events_reach_stderr_only() {
        let observed = observe("trace", "trace", "trace", LogFormat::Text, || {
            let span = tracing::info_span!(target: "okf_mcp::test", "outer");
            let _entered = span.enter();
            tracing::error!(target: LOCAL_ONLY, "local-error-line");
            tracing::warn!(target: LOCAL_ONLY, "local-warn-line");
            tracing::info!(target: LOCAL_ONLY, "local-info-line");
            tracing::debug!(target: LOCAL_ONLY, "local-debug-line");
            tracing::trace!(target: LOCAL_ONLY, "local-trace-line");
        });
        for level in ["error", "warn", "info", "debug", "trace"] {
            let line = format!("local-{level}-line");
            assert!(
                observed.stderr.contains(&line),
                "{line}: {}",
                observed.stderr
            );
        }
        assert!(observed.logs.is_empty(), "{:?}", observed.logs);
        let outer = observed
            .spans
            .iter()
            .find(|span| span.name == "outer")
            .expect("outer span");
        assert!(outer.events.is_empty(), "no span event: {:?}", outer.events);
        assert_eq!(
            outer.status,
            Status::Unset,
            "a local-only error does not mark the span"
        );
    }

    /// Emits an event at every level and a span, all with `$target`.
    macro_rules! emit_everywhere {
        ($target:literal) => {{
            let span = tracing::info_span!(target: $target, "dep_span");
            let _entered = span.enter();
            tracing::error!(target: $target, "dep-event");
            tracing::warn!(target: $target, "dep-event");
            tracing::info!(target: $target, "dep-event");
            tracing::debug!(target: $target, "dep-event");
            tracing::trace!(target: $target, "dep-event");
        }};
    }

    #[test]
    fn dependency_events_never_reach_otlp() {
        let observed = observe("trace", "trace", "trace", LogFormat::Text, || {
            emit_everywhere!("sqlx::query");
            emit_everywhere!("apalis");
            emit_everywhere!("openidconnect");
            emit_everywhere!("aws_config");
            emit_everywhere!("tower_http");
            emit_everywhere!("rmcp");
            tracing::info!(target: "okf_mcp::store", "own-event");
            let span = tracing::info_span!(target: "okf_mcp::store", "own_span");
            drop(span);
        });
        assert!(
            observed.logs.iter().all(|log| !log.contains("dep-event")),
            "{:?}",
            observed.logs
        );
        assert!(
            observed.logs.iter().any(|log| log.contains("own-event")),
            "{:?}",
            observed.logs
        );
        assert!(observed.spans.iter().all(|span| span.name != "dep_span"));
        assert!(observed.spans.iter().any(|span| span.name == "own_span"));
        // stderr still sees the dependencies (rmcp capped at info).
        assert!(observed.stderr.contains("dep-event"), "{}", observed.stderr);
    }

    #[test]
    fn error_event_marks_span_error() {
        let observed = observe("warn", "info", "info", LogFormat::Text, || {
            let span = tracing::info_span!(target: "okf_mcp::test", "failing");
            let _entered = span.enter();
            tracing::error!(target: "okf_mcp::test", "it failed");
        });
        let span = observed
            .spans
            .iter()
            .find(|span| span.name == "failing")
            .expect("span");
        // The status is Error with no description, and no error text became
        // an exception attribute: the privacy rules keep error text out.
        assert_eq!(span.status, Status::error(""));
        assert!(
            !span
                .attributes
                .iter()
                .any(|kv| kv.key.as_str().starts_with("exception")),
            "{:?}",
            span.attributes
        );
    }

    #[test]
    fn settings_prefer_rust_log_then_config_then_default() {
        let mut config = TelemetryConfig::default();
        config.stderr.filter = Some("debug".to_string());
        let from_env = settings(&config, Some("error")).expect("settings");
        assert_eq!(from_env.stderr.to_string(), "error");
        let from_config = settings(&config, Some("=[")).expect("invalid RUST_LOG falls back");
        assert_eq!(from_config.stderr.to_string(), "debug");
        let blank = settings(&config, Some("  ")).expect("blank RUST_LOG falls back");
        assert_eq!(blank.stderr.to_string(), "debug");
        config.stderr.filter = None;
        let default = settings(&config, None).expect("settings");
        assert_eq!(
            default.stderr.to_string(),
            parse_filter(DEFAULT_STDERR_FILTER).unwrap().to_string()
        );
        assert_eq!(default.traces.to_string(), "info");
    }

    #[test]
    fn the_fallback_writes_text_at_the_default_filter() {
        let capture = Capture::default();
        let subscriber = fallback(None, capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "okf_mcp::test", "fallback-info-line");
            tracing::debug!(target: "okf_mcp::test", "fallback-debug-line");
            tracing::info!(target: "somewhere_else", "fallback-dependency-info");
        });
        let text = capture.text();
        assert!(text.contains("fallback-info-line"), "{text}");
        assert!(!text.contains("fallback-debug-line"), "{text}");
        assert!(!text.contains("fallback-dependency-info"), "{text}");
    }
}

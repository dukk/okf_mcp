//! stderr's JSON line format, with the trace and span ids of the current
//! span.
//!
//! `tracing-subscriber`'s own JSON formatter has no trace id, and
//! `tracing-opentelemetry` keeps its span data private. Its public path is
//! [`tracing_opentelemetry::get_otel_context`], which needs the subscriber's
//! `Dispatch`. A formatter is not given one, and `tracing::dispatcher::
//! get_default` answers nothing under a scoped dispatcher, so a no-op layer
//! ([`CaptureDispatch`]) stores a weak handle to it when the subscriber is
//! installed.

use std::fmt;
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value};
use tracing::dispatcher::WeakDispatch;
use tracing::field::{Field, Visit};
use tracing::{Dispatch, Event, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Where [`CaptureDispatch`] leaves the subscriber's `Dispatch`, and
/// [`JsonWithTrace`] finds it. Cloning shares the slot.
#[derive(Debug, Clone, Default)]
pub struct DispatchSlot(Arc<OnceLock<WeakDispatch>>);

/// A layer that does nothing but remember the subscriber it is part of.
#[derive(Debug)]
pub struct CaptureDispatch(pub DispatchSlot);

impl<S: Subscriber> Layer<S> for CaptureDispatch {
    // Called once, when the `Dispatch` wrapping the finished subscriber is
    // created. A weak handle does not keep the subscriber alive, so there is
    // no reference cycle (the subscriber owns this layer).
    fn on_register_dispatch(&self, subscriber: &Dispatch) {
        // A second registration (the subscriber wrapped twice) keeps the
        // first; both are the same subscriber.
        let _ = (self.0).0.set(subscriber.downgrade());
    }
}

/// One JSON object per event: `timestamp`, `level`, `target`, `message`, the
/// event's own fields, and `trace_id` and `span_id` when the event is inside
/// a span the OTel layer is tracing.
#[derive(Debug, Clone)]
pub struct JsonWithTrace(pub DispatchSlot);

/// Collects an event's fields as JSON values, so numbers stay numbers.
struct FieldCollector<'a>(&'a mut Map<String, Value>);

impl Visit for FieldCollector<'_> {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        // `Value::from(f64)` is `Null` for NaN and infinities, which JSON
        // cannot hold.
        self.0.insert(field.name().to_string(), Value::from(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0
            .insert(field.name().to_string(), Value::from(format!("{value:?}")));
    }
}

impl<S, N> FormatEvent<S, N> for JsonWithTrace
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let metadata = event.metadata();
        let mut object = Map::new();
        // The event's fields go in first, so the fixed keys below win over a
        // field that happens to share a name (a `trace_id` field cannot spoof
        // the real one).
        event.record(&mut FieldCollector(&mut object));
        object.insert(
            "timestamp".to_string(),
            Value::from(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)),
        );
        object.insert("level".to_string(), Value::from(metadata.level().as_str()));
        object.insert("target".to_string(), Value::from(metadata.target()));
        if let Some((trace_id, span_id)) = self.current_ids(ctx) {
            object.insert("trace_id".to_string(), Value::from(trace_id));
            object.insert("span_id".to_string(), Value::from(span_id));
        }
        let line = serde_json::to_string(&Value::Object(object)).map_err(|_| fmt::Error)?;
        writeln!(writer, "{line}")
    }
}

impl JsonWithTrace {
    /// The trace and span ids (lower-case hex) of the event's current span,
    /// when the OTel layer is tracing it.
    fn current_ids<S, N>(&self, ctx: &FmtContext<'_, S, N>) -> Option<(String, String)>
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
        N: for<'writer> FormatFields<'writer> + 'static,
    {
        use opentelemetry::trace::TraceContextExt;

        let span = ctx.lookup_current()?;
        let dispatch = self.0 .0.get()?.upgrade()?;
        // Not under `ExtensionsMut`: `get_otel_context` locks the span's
        // extensions itself.
        let context = tracing_opentelemetry::get_otel_context(&span.id(), &dispatch)?;
        let span_context = context.span().span_context().clone();
        span_context.is_valid().then(|| {
            (
                span_context.trace_id().to_string(),
                span_context.span_id().to_string(),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use serde_json::Value;

    use super::super::config::{parse_filter, LogFormat};
    use super::super::subscriber::capture::Capture;
    use super::super::subscriber::{build_with_writer, LayerSettings};

    /// Runs `body` under the JSON subscriber and returns the lines written.
    fn lines(body: impl FnOnce()) -> Vec<Value> {
        let tracer = SdkTracerProvider::builder()
            .with_simple_exporter(InMemorySpanExporter::default())
            .build();
        let capture = Capture::default();
        let settings = LayerSettings {
            format: LogFormat::Json,
            stderr: parse_filter("trace").expect("filter"),
            traces: parse_filter("trace").expect("filter"),
            logs: parse_filter("trace").expect("filter"),
        };
        let subscriber = build_with_writer(settings, Some(&tracer), None, capture.clone());
        tracing::subscriber::with_default(subscriber, body);
        let _ = tracer.shutdown();
        capture
            .text()
            .lines()
            .map(|line| serde_json::from_str(line).expect("each line is JSON"))
            .collect()
    }

    fn is_hex(value: &Value, len: usize) -> bool {
        value
            .as_str()
            .is_some_and(|text| text.len() == len && text.bytes().all(|b| b.is_ascii_hexdigit()))
    }

    #[test]
    fn an_event_in_a_span_carries_trace_and_span_ids() {
        let lines = lines(|| {
            let span = tracing::info_span!(target: "okf_mcp::test", "work");
            let _entered = span.enter();
            tracing::info!(target: "okf_mcp::test", count = 3_u64, ok = true, "inside");
        });
        let [line] = lines.as_slice() else {
            panic!("one line, got {lines:?}");
        };
        assert!(is_hex(&line["trace_id"], 32), "{line}");
        assert!(is_hex(&line["span_id"], 16), "{line}");
        assert_eq!(line["message"], "inside");
        assert_eq!(line["level"], "INFO");
        assert_eq!(line["target"], "okf_mcp::test");
        // Numbers and booleans stay JSON numbers and booleans.
        assert_eq!(line["count"], 3);
        assert_eq!(line["ok"], true);
        assert!(
            line["timestamp"]
                .as_str()
                .is_some_and(|text| text.ends_with('Z')),
            "{line}"
        );
    }

    #[test]
    fn an_event_outside_any_span_has_no_trace_id() {
        let lines = lines(|| tracing::info!(target: "okf_mcp::test", "outside"));
        let [line] = lines.as_slice() else {
            panic!("one line, got {lines:?}");
        };
        assert!(line.get("trace_id").is_none(), "{line}");
        assert!(line.get("span_id").is_none(), "{line}");
    }

    #[test]
    fn a_field_cannot_spoof_the_trace_id() {
        let lines = lines(|| {
            let span = tracing::info_span!(target: "okf_mcp::test", "work");
            let _entered = span.enter();
            tracing::info!(target: "okf_mcp::test", trace_id = "forged", level = "forged", "x");
        });
        assert!(is_hex(&lines[0]["trace_id"], 32), "{}", lines[0]);
        assert_eq!(lines[0]["level"], "INFO");
    }
}

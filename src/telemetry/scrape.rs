//! The `/metrics` listener: the Prometheus text exposition of the registry
//! the Prometheus reader renders into, on its own port (spec Decision 2).
//!
//! It has no authentication and the ingress never routes to it; the
//! operator reaches it through the cluster network only.

use super::subscriber::split_event;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use prometheus::proto::MetricFamily;
use prometheus::{Encoder, Registry, TextEncoder};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Serves `GET /metrics` on `listener` until `shutdown` is cancelled.
///
/// Every other path is a 404.
///
/// # Arguments
/// * `listener` - the bound socket.
/// * `registry` - what the Prometheus reader renders into.
/// * `shutdown` - cancelled to stop accepting and drain.
///
/// # Errors
/// The listener's I/O error.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "long-lived loop: its span would never close and would stay open as the parent of every later span"
    )
)]
pub async fn serve(
    listener: TcpListener,
    registry: Registry,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    axum::serve(listener, router(registry))
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await
}

/// Where a scrape reads its families from: the registry's `gather`, or a
/// stand-in in tests. `Arc<dyn Fn..>` is a shared, type-erased callable (a
/// Python function value), so the router state is one concrete type.
type Gather = Arc<dyn Fn() -> Vec<MetricFamily> + Send + Sync>;

/// The router: `/metrics` over `registry`.
#[tracing::instrument(level = "trace", skip_all)]
fn router(registry: Registry) -> Router {
    router_with(Arc::new(move || registry.gather()))
}

/// The router over any source of families.
#[tracing::instrument(level = "trace", skip_all)]
fn router_with(gather: Gather) -> Router {
    Router::new()
        .route("/metrics", get(render))
        .with_state(gather)
}

/// Encodes the whole exposition into a buffer, then answers with it.
///
/// The body is complete before any byte is sent, so an encoder error never
/// leaves a scraper a partial exposition: it is a 500 with no body.
#[tracing::instrument(level = "debug", skip_all)]
async fn render(State(gather): State<Gather>) -> Response {
    let mut body = Vec::new();
    let encoder = TextEncoder::new();
    match encoder.encode(&gather(), &mut body) {
        Ok(()) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, encoder.format_type().to_string())],
            body,
        )
            .into_response(),
        Err(error) => {
            split_event!(error, {}, { %error, }, "encoding the metrics failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::time::Duration;

    use opentelemetry::metrics::Meter;
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry::KeyValue;
    use opentelemetry_sdk::metrics::SdkMeterProvider;
    use prometheus::proto::MetricFamily;

    use super::*;
    use crate::telemetry::metrics::{observe_export_failures, seconds_histogram, Metrics};
    use crate::telemetry::otlp::ExportFailures;

    /// A provider with only the Prometheus reader, shaped as `serve` builds
    /// it, and the registry it renders into.
    fn test_meter() -> (SdkMeterProvider, Meter, Registry) {
        let registry = Registry::new();
        let reader = opentelemetry_prometheus::exporter()
            .with_registry(registry.clone())
            .without_target_info()
            .scope_info_enabled(false)
            .build()
            .expect("reader");
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let meter = provider.meter("okf-mcp");
        (provider, meter, registry)
    }

    /// Binds a loopback listener and serves `registry` on it.
    async fn serving(
        registry: Registry,
    ) -> (
        String,
        CancellationToken,
        tokio::task::JoinHandle<std::io::Result<()>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let shutdown = CancellationToken::new();
        let served = tokio::spawn(serve(listener, registry, shutdown.clone()));
        (format!("http://{address}"), shutdown, served)
    }

    /// Stops the server and waits for it, under a timeout.
    async fn stop(
        shutdown: CancellationToken,
        served: tokio::task::JoinHandle<std::io::Result<()>>,
    ) {
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(10), served)
            .await
            .expect("the listener stops within 10 s")
            .expect("join")
            .expect("serves");
    }

    #[tokio::test]
    async fn every_family_is_present_before_any_request() {
        let (_provider, meter, registry) = test_meter();
        let _metrics = Metrics::new(&meter);
        observe_export_failures(&meter, Arc::new(ExportFailures::default()));
        let (base, shutdown, served) = serving(registry).await;

        let response = reqwest::get(format!("{base}/metrics")).await.expect("get");
        assert_eq!(response.status(), 200);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = response.text().await.expect("body");

        assert!(
            content_type.starts_with("text/plain; version=0.0.4"),
            "{content_type}"
        );
        for family in Metrics::FAMILIES {
            assert!(
                body.contains(&format!("# TYPE {family} ")),
                "{family} missing:\n{body}"
            );
        }
        assert!(!body.contains("target_info") && !body.contains("otel_scope_"));
        assert!(body.contains(&format!("version=\"{}\"", env!("CARGO_PKG_VERSION"))));
        // A family declared twice makes Prometheus reject the whole scrape.
        let types: Vec<&str> = body
            .lines()
            .filter(|line| line.starts_with("# TYPE "))
            .collect();
        let unique: BTreeSet<&&str> = types.iter().collect();
        assert_eq!(types.len(), unique.len(), "{body}");
        // One series per signal and reason, all zero.
        assert_eq!(
            body.lines()
                .filter(|line| line.starts_with("okf_telemetry_export_failures_total{"))
                .count(),
            12,
            "{body}"
        );
        stop(shutdown, served).await;
    }

    #[tokio::test]
    async fn a_counted_failure_shows_on_the_next_scrape() {
        use crate::telemetry::otlp::{FailureReason, Signal};
        let (_provider, meter, registry) = test_meter();
        let failures = Arc::new(ExportFailures::default());
        observe_export_failures(&meter, Arc::clone(&failures));
        failures.record(Signal::Logs, FailureReason::Rejected);
        let (base, shutdown, served) = serving(registry).await;

        let body = reqwest::get(format!("{base}/metrics"))
            .await
            .expect("get")
            .text()
            .await
            .expect("body");

        assert!(
            body.contains(
                "okf_telemetry_export_failures_total{reason=\"rejected\",signal=\"logs\"} 1"
            ),
            "{body}"
        );
        stop(shutdown, served).await;
    }

    #[tokio::test]
    async fn other_paths_are_404() {
        let (_provider, _meter, registry) = test_meter();
        let (base, shutdown, served) = serving(registry).await;

        for path in ["/", "/metrics/", "/healthz", "/metricsx"] {
            let status = reqwest::get(format!("{base}{path}"))
                .await
                .expect("get")
                .status();
            assert_eq!(status, 404, "{path}");
        }
        // Only GET is routed.
        let post = reqwest::Client::new()
            .post(format!("{base}/metrics"))
            .send()
            .await
            .expect("post");
        assert_eq!(post.status(), 405);
        stop(shutdown, served).await;
    }

    #[tokio::test]
    async fn seconds_histograms_use_the_seconds_buckets() {
        let (_provider, meter, registry) = test_meter();
        let histogram = seconds_histogram(&meter, "okf.mcp.tool.duration", "Tool call time.");
        histogram.record(0.042, &[KeyValue::new("tool", "search")]);
        let (base, shutdown, served) = serving(registry).await;

        let body = reqwest::get(format!("{base}/metrics"))
            .await
            .expect("get")
            .text()
            .await
            .expect("body");

        assert!(
            body.contains("# TYPE okf_mcp_tool_duration_seconds histogram"),
            "{body}"
        );
        // 42 ms is above the 25 ms bound and inside the 50 ms one.
        assert!(
            body.contains("okf_mcp_tool_duration_seconds_bucket{tool=\"search\",le=\"0.025\"} 0"),
            "{body}"
        );
        assert!(
            body.contains("okf_mcp_tool_duration_seconds_bucket{tool=\"search\",le=\"0.05\"} 1"),
            "{body}"
        );
        assert!(
            body.contains("okf_mcp_tool_duration_seconds_bucket{tool=\"search\",le=\"60\"} 1"),
            "{body}"
        );
        stop(shutdown, served).await;
    }

    #[tokio::test]
    async fn the_scrape_never_writes_a_partial_body() {
        // A good family followed by one the encoder refuses (no metrics):
        // the encoder has already written the first into its buffer when it
        // fails, and none of it may reach the scraper.
        let gather: Gather = Arc::new(|| {
            let counter = prometheus::IntCounter::new("fine_total", "fine").expect("counter");
            let mut families = prometheus::core::Collector::collect(&counter);
            let mut empty = MetricFamily::default();
            empty.set_name("empty".to_string());
            families.push(empty);
            families
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let shutdown = CancellationToken::new();
        let stopping = shutdown.clone();
        let served = tokio::spawn(async move {
            axum::serve(listener, router_with(gather))
                .with_graceful_shutdown(async move { stopping.cancelled().await })
                .await
        });

        let response = reqwest::get(format!("http://{address}/metrics"))
            .await
            .expect("get");

        assert_eq!(response.status(), 500);
        assert!(response.text().await.expect("body").is_empty());
        stop(shutdown, served).await;
    }
}

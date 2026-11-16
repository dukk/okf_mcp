//! An in-process OTLP/HTTP collector for the telemetry tests.
//!
//! It listens on a free loopback port, decodes the protobuf bodies the
//! exporters post, and keeps every request: its path, its headers, its raw
//! body and the decoded message. It can be told to answer 500 or to redirect,
//! so the tests can watch the exporters' failure paths. It serves any path
//! that ends in `/v1/traces`, `/v1/logs` or `/v1/metrics`, which covers the
//! plain endpoint and the Sentry-style `/api/<project>/integration/otlp/...`
//! prefix alike.

// Each test crate that includes `support` uses a different part of it.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{any_value, AnyValue, KeyValue};
use opentelemetry_proto::tonic::logs::v1::LogRecord;
use opentelemetry_proto::tonic::metrics::v1::{metric, Metric};
use opentelemetry_proto::tonic::trace::v1::Span;
use prost::Message;
use tokio::task::JoinHandle;

/// How the collector answers the next requests.
#[derive(Debug, Clone)]
pub enum Mode {
    /// 200 with an empty OTLP response.
    Accept,
    /// 500.
    Fail500,
    /// 307 to this absolute URL.
    Redirect(String),
}

/// One decoded export request.
#[derive(Debug, Clone)]
pub enum Payload {
    /// `POST .../v1/traces`.
    Traces(ExportTraceServiceRequest),
    /// `POST .../v1/logs`.
    Logs(ExportLogsServiceRequest),
    /// `POST .../v1/metrics`.
    Metrics(ExportMetricsServiceRequest),
    /// A path that is not one of the three, or a body that did not decode.
    Undecodable(String),
}

/// One request the collector received.
#[derive(Debug, Clone)]
pub struct Recorded {
    /// The request path (no query), for example `/v1/traces`.
    pub path: String,
    /// Every header, with a lowercase name.
    pub headers: Vec<(String, String)>,
    /// The body exactly as it arrived (protobuf, never compressed here).
    pub body: Vec<u8>,
    /// The body, decoded.
    pub payload: Payload,
}

impl Recorded {
    /// The first header called `name`.
    ///
    /// # Arguments
    /// * `name` - a lowercase header name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// What the handler shares with the test.
struct Shared {
    requests: Mutex<Vec<Recorded>>,
    mode: Mutex<Mode>,
}

/// A running collector. Dropping it stops the server.
pub struct Collector {
    addr: SocketAddr,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Collector {
    /// Starts a collector on a free loopback port, answering 200.
    ///
    /// # Panics
    /// If the bind fails: a broken test setup.
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the collector");
        let addr = listener.local_addr().expect("collector address");
        let shared = Arc::new(Shared {
            requests: Mutex::new(Vec::new()),
            mode: Mutex::new(Mode::Accept),
        });
        let router = Router::new()
            .fallback(receive)
            .with_state(Arc::clone(&shared));
        let task = tokio::spawn(async move {
            // The server ends when the task is aborted on drop.
            let _ = axum::serve(listener, router).await;
        });
        Self {
            addr,
            shared,
            task,
        }
    }

    /// The collector's base URL, with no trailing slash.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The collector's `host:port`.
    pub fn authority(&self) -> String {
        self.addr.to_string()
    }

    /// Changes how later requests are answered.
    pub fn set_mode(&self, mode: Mode) {
        *self.shared.mode.lock().expect("mode lock") = mode;
    }

    /// Every request received so far, oldest first.
    pub fn requests(&self) -> Vec<Recorded> {
        self.shared.requests.lock().expect("requests lock").clone()
    }

    /// Polls `condition` over the requests received so far until it holds.
    ///
    /// It never sleeps blindly: it checks every 25 ms and returns at once
    /// when the condition holds.
    ///
    /// # Arguments
    /// * `what` - what is awaited, for the failure message.
    /// * `condition` - true once the requests are enough.
    /// * `timeout` - the longest to wait.
    ///
    /// # Returns
    /// The requests at the moment the condition held.
    ///
    /// # Panics
    /// If the condition does not hold within `timeout`.
    pub async fn wait_for(
        &self,
        what: &str,
        condition: impl Fn(&[Recorded]) -> bool,
        timeout: Duration,
    ) -> Vec<Recorded> {
        let deadline = Instant::now() + timeout;
        loop {
            let requests = self.requests();
            if condition(&requests) {
                return requests;
            }
            assert!(
                Instant::now() < deadline,
                "timed out after {timeout:?} waiting for {what}; the collector saw: {}",
                summarize(&requests)
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

/// One line per request: path and message kind.
fn summarize(requests: &[Recorded]) -> String {
    let lines: Vec<String> = requests
        .iter()
        .map(|request| {
            let kind = match &request.payload {
                Payload::Traces(_) => "traces",
                Payload::Logs(_) => "logs",
                Payload::Metrics(_) => "metrics",
                Payload::Undecodable(_) => "undecodable",
            };
            format!("{} ({kind}, {} bytes)", request.path, request.body.len())
        })
        .collect();
    format!("[{}]", lines.join(", "))
}

/// Records the request and answers per the current [`Mode`].
async fn receive(
    State(shared): State<Arc<Shared>>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let payload = decode(&path, &body);
    let recorded = Recorded {
        path: path.clone(),
        headers: headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect(),
        body: body.to_vec(),
        payload,
    };
    shared.requests.lock().expect("requests lock").push(recorded);
    let mode = shared.mode.lock().expect("mode lock").clone();
    match mode {
        Mode::Accept => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/x-protobuf")],
            Vec::<u8>::new(),
        )
            .into_response(),
        Mode::Fail500 => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Mode::Redirect(to) => (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, format!("{to}{path}"))],
        )
            .into_response(),
    }
}

/// Decodes `body` as the message its path names.
fn decode(path: &str, body: &[u8]) -> Payload {
    let undecodable = |error: prost::DecodeError| Payload::Undecodable(error.to_string());
    if path.ends_with("/v1/traces") {
        ExportTraceServiceRequest::decode(body).map_or_else(undecodable, Payload::Traces)
    } else if path.ends_with("/v1/logs") {
        ExportLogsServiceRequest::decode(body).map_or_else(undecodable, Payload::Logs)
    } else if path.ends_with("/v1/metrics") {
        ExportMetricsServiceRequest::decode(body).map_or_else(undecodable, Payload::Metrics)
    } else {
        Payload::Undecodable(format!("no OTLP signal at {path}"))
    }
}

/// Every span in `requests`.
pub fn spans(requests: &[Recorded]) -> Vec<&Span> {
    requests
        .iter()
        .filter_map(|request| match &request.payload {
            Payload::Traces(message) => Some(message),
            _ => None,
        })
        .flat_map(|message| &message.resource_spans)
        .flat_map(|resource| &resource.scope_spans)
        .flat_map(|scope| &scope.spans)
        .collect()
}

/// Every log record in `requests`.
pub fn logs(requests: &[Recorded]) -> Vec<&LogRecord> {
    requests
        .iter()
        .filter_map(|request| match &request.payload {
            Payload::Logs(message) => Some(message),
            _ => None,
        })
        .flat_map(|message| &message.resource_logs)
        .flat_map(|resource| &resource.scope_logs)
        .flat_map(|scope| &scope.log_records)
        .collect()
}

/// Every metric (a name with its points) in `requests`.
pub fn metrics(requests: &[Recorded]) -> Vec<&Metric> {
    requests
        .iter()
        .filter_map(|request| match &request.payload {
            Payload::Metrics(message) => Some(message),
            _ => None,
        })
        .flat_map(|message| &message.resource_metrics)
        .flat_map(|resource| &resource.scope_metrics)
        .flat_map(|scope| &scope.metrics)
        .collect()
}

/// How many data points `metric` holds.
pub fn point_count(metric: &Metric) -> usize {
    match &metric.data {
        Some(metric::Data::Gauge(gauge)) => gauge.data_points.len(),
        Some(metric::Data::Sum(sum)) => sum.data_points.len(),
        Some(metric::Data::Histogram(histogram)) => histogram.data_points.len(),
        Some(metric::Data::ExponentialHistogram(histogram)) => histogram.data_points.len(),
        Some(metric::Data::Summary(summary)) => summary.data_points.len(),
        None => 0,
    }
}

/// An attribute's value as text, or `None` when there is no such key.
///
/// # Arguments
/// * `attributes` - a span's, event's or record's attributes.
/// * `key` - the attribute's name.
pub fn attribute(attributes: &[KeyValue], key: &str) -> Option<String> {
    attributes
        .iter()
        .find(|pair| pair.key == key)
        .map(|pair| pair.value.as_ref().map(any_text).unwrap_or_default())
}

/// A value rendered as text (a string as it is, a number or flag in its
/// usual form, anything composite through `Debug`).
pub fn any_text(value: &AnyValue) -> String {
    match &value.value {
        Some(any_value::Value::StringValue(text)) => text.clone(),
        Some(any_value::Value::IntValue(number)) => number.to_string(),
        Some(any_value::Value::DoubleValue(number)) => number.to_string(),
        Some(any_value::Value::BoolValue(flag)) => flag.to_string(),
        other => format!("{other:?}"),
    }
}

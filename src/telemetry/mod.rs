//! Observability for `serve`: the OTLP span and log providers, the
//! subscriber that feeds them and stderr, and shutdown.
//!
//! Everything else in the crate uses `tracing` macros only. Telemetry starts
//! for `serve` alone; the other subcommands get the stderr layer (see
//! [`install`]).

pub mod config;
pub mod json;
pub mod otlp;
pub mod subscriber;

use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use opentelemetry::global;
use opentelemetry::KeyValue;
use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::logs::log_processor_with_async_runtime::BatchLogProcessor;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::runtime;
use opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use opentelemetry_sdk::Resource;
use tracing_subscriber::fmt::writer::MakeWriter;

use crate::config::Config;
use crate::error::OkfError;
use config::TelemetryConfig;
use otlp::{ExportFailures, OtlpTarget};
use subscriber::LayerSettings;

/// How long shutdown waits for the last flushes (inside Kubernetes' default
/// 30 s grace, after the HTTP drain).
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(4);

/// How one provider fared at shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderOutcome {
    /// The provider was never started (that signal is off).
    NotRunning,
    /// Flushed and shut down (or had already been).
    Flushed,
    /// The flush failed, with the SDK's message (which carries no payload).
    Failed(String),
    /// Still running at the deadline, and left behind.
    Abandoned,
}

/// What [`Telemetry::shutdown`] did, one outcome per provider. Task 7's
/// sentinel test asserts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    /// The span provider.
    pub traces: ProviderOutcome,
    /// The log provider.
    pub logs: ProviderOutcome,
}

impl ShutdownReport {
    /// `true` when every provider that was running flushed.
    pub fn all_flushed(&self) -> bool {
        [&self.traces, &self.logs].iter().all(|outcome| {
            matches!(
                outcome,
                ProviderOutcome::NotRunning | ProviderOutcome::Flushed
            )
        })
    }
}

/// The running telemetry of `serve`: the providers, kept for shutdown.
#[derive(Debug)]
pub struct Telemetry {
    tracer: Option<SdkTracerProvider>,
    logger: Option<SdkLoggerProvider>,
    failures: Arc<ExportFailures>,
}

/// The resource every signal carries.
///
/// `Resource::builder` runs the SDK's detectors, so `OTEL_RESOURCE_ATTRIBUTES`
/// adds keys such as `k8s.pod.name`; what is set here in code overrides the
/// same key from the environment.
fn resource(config: &TelemetryConfig) -> Resource {
    let mut attributes = vec![KeyValue::new("service.version", env!("CARGO_PKG_VERSION"))];
    if let Some(environment) = &config.environment {
        attributes.push(KeyValue::new(
            "deployment.environment.name",
            environment.clone(),
        ));
    }
    Resource::builder()
        .with_service_name(config.service_name.clone())
        .with_attributes(attributes)
        .build()
}

impl Telemetry {
    /// Builds the resource, the exporters and the providers for `serve`, sets
    /// the global tracer provider and the W3C propagator. An exporter that
    /// cannot be built is logged (to stderr) and its signal stays off.
    ///
    /// Must run inside a tokio runtime: the batch processors spawn their
    /// worker on it.
    ///
    /// # Arguments
    /// * `config` - `[telemetry]`, already validated.
    /// * `env` - reads an environment variable (injected for tests).
    ///
    /// # Returns
    /// The running telemetry; with no `[telemetry.otlp]` (or
    /// `OTEL_SDK_DISABLED=true`) it holds no providers.
    ///
    /// # Errors
    /// [`OkfError::Config`] when a variable named by `headers_env` or
    /// `sentry_dsn_env` is unset or blank, or its value does not parse.
    pub fn start(
        config: &TelemetryConfig,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, OkfError> {
        let failures = Arc::new(ExportFailures::default());
        let Some(otlp) = &config.otlp else {
            return Ok(Self {
                tracer: None,
                logger: None,
                failures,
            });
        };
        // Before the target is resolved: a disabled SDK never needs the
        // credentials, so removing them must not stop the service starting.
        if env("OTEL_SDK_DISABLED").is_some_and(|value| value.trim().eq_ignore_ascii_case("true")) {
            tracing::info!("OTLP export is off: OTEL_SDK_DISABLED is true");
            return Ok(Self {
                tracer: None,
                logger: None,
                failures,
            });
        }
        let target = OtlpTarget::resolve(otlp, env)?;
        let resource = resource(config);

        let tracer = match otlp::span_exporter(&target, &failures) {
            Ok(exporter) => {
                let processor = BatchSpanProcessor::builder(exporter, runtime::Tokio).build();
                let mut builder = SdkTracerProvider::builder()
                    .with_resource(resource.clone())
                    .with_span_processor(processor);
                if let Some(ratio) = otlp.sample_ratio {
                    builder = builder.with_sampler(Sampler::ParentBased(Box::new(
                        Sampler::TraceIdRatioBased(ratio),
                    )));
                }
                Some(builder.build())
            }
            Err(error) => {
                tracing::warn!("OTLP traces are off: {error}");
                None
            }
        };
        let logger = match otlp::log_exporter(&target, &failures) {
            Ok(exporter) => {
                let processor = BatchLogProcessor::builder(exporter, runtime::Tokio).build();
                Some(
                    SdkLoggerProvider::builder()
                        .with_resource(resource)
                        .with_log_processor(processor)
                        .build(),
                )
            }
            Err(error) => {
                tracing::warn!("OTLP logs are off: {error}");
                None
            }
        };
        if let Some(provider) = &tracer {
            global::set_tracer_provider(provider.clone());
            global::set_text_map_propagator(TraceContextPropagator::new());
        }
        Ok(Self {
            tracer,
            logger,
            failures,
        })
    }

    /// Wraps providers that were built elsewhere (tests with in-memory
    /// exporters, or a hung one).
    #[cfg(test)]
    fn from_providers(
        tracer: Option<SdkTracerProvider>,
        logger: Option<SdkLoggerProvider>,
    ) -> Self {
        Self {
            tracer,
            logger,
            failures: Arc::new(ExportFailures::default()),
        }
    }

    /// Whether OTLP spans are being exported.
    pub fn traces_enabled(&self) -> bool {
        self.tracer.is_some()
    }

    /// Whether OTLP logs are being exported.
    pub fn logs_enabled(&self) -> bool {
        self.logger.is_some()
    }

    /// The failed-export counts the exporters keep.
    pub fn failures(&self) -> &Arc<ExportFailures> {
        &self.failures
    }

    /// The subscriber for `serve`: stderr plus whichever OTLP layers started.
    ///
    /// # Arguments
    /// * `settings` - the resolved filters and format.
    pub fn subscriber(&self, settings: LayerSettings) -> impl tracing::Subscriber + Send + Sync {
        subscriber::build(settings, self.tracer.as_ref(), self.logger.as_ref())
    }

    /// Flushes and shuts down every provider in parallel, each on a plain
    /// thread, within [`SHUTDOWN_DEADLINE`]. A flush still running at the
    /// deadline is abandoned and named on stderr.
    ///
    /// # Returns
    /// Each provider's outcome. A provider that was already shut down counts
    /// as flushed, and says nothing.
    pub async fn shutdown(self) -> ShutdownReport {
        self.shutdown_within(SHUTDOWN_DEADLINE).await
    }

    /// [`Telemetry::shutdown`] with the deadline as a parameter.
    async fn shutdown_within(self, deadline: Duration) -> ShutdownReport {
        let Self { tracer, logger, .. } = self;
        // `spawn_blocking` moves the waiting off the async workers: it blocks
        // on a channel until the providers answer or the deadline passes.
        tokio::task::spawn_blocking(move || shutdown_blocking(tracer, logger, deadline))
            .await
            .unwrap_or_else(|_| ShutdownReport {
                traces: ProviderOutcome::Failed("the shutdown task failed".to_string()),
                logs: ProviderOutcome::Failed("the shutdown task failed".to_string()),
            })
    }
}

/// Shuts the providers down, each on its own thread, and waits for them
/// until `deadline`.
///
/// Plain threads, not tasks: the tokio batch processor ignores the timeout
/// given to `shutdown_with_timeout`, and a dead receiver costs several retry
/// timeouts.
fn shutdown_blocking(
    tracer: Option<SdkTracerProvider>,
    logger: Option<SdkLoggerProvider>,
    deadline: Duration,
) -> ShutdownReport {
    /// Which provider a finished thread was.
    #[derive(Clone, Copy, PartialEq)]
    enum Which {
        Traces,
        Logs,
    }
    let (sender, receiver) = mpsc::channel::<(Which, Result<(), OTelSdkError>)>();
    let mut pending = Vec::new();
    let mut spawn =
        |which: Which,
         name: &'static str,
         work: Box<dyn FnOnce() -> Result<(), OTelSdkError> + Send>| {
            let sender = sender.clone();
            let started = std::thread::Builder::new()
                .name(format!("telemetry-shutdown-{name}"))
                .spawn(move || {
                    // A closed channel means the deadline passed; nobody is listening.
                    let _ = sender.send((which, work()));
                });
            match started {
                Ok(_) => pending.push((which, name)),
                Err(error) => {
                    tracing::warn!(provider = name, %error, "telemetry shutdown thread did not start")
                }
            }
        };
    if let Some(provider) = tracer {
        spawn(
            Which::Traces,
            "traces",
            Box::new(move || provider.shutdown()),
        );
    }
    if let Some(provider) = logger {
        spawn(Which::Logs, "logs", Box::new(move || provider.shutdown()));
    }
    drop(sender);

    let mut outcomes: Vec<(Which, ProviderOutcome)> = Vec::new();
    let started = Instant::now();
    while outcomes.len() < pending.len() {
        let remaining = deadline.saturating_sub(started.elapsed());
        match receiver.recv_timeout(remaining) {
            Ok((which, result)) => outcomes.push((
                which,
                match result {
                    Ok(()) | Err(OTelSdkError::AlreadyShutdown) => ProviderOutcome::Flushed,
                    Err(error) => ProviderOutcome::Failed(error.to_string()),
                },
            )),
            Err(_) => break,
        }
    }
    let outcome = |which: Which, name: &str, was_started: bool| {
        if let Some((_, outcome)) = outcomes.iter().find(|(found, _)| *found == which) {
            return outcome.clone();
        }
        if !was_started {
            return ProviderOutcome::NotRunning;
        }
        // It reaches stderr: the stderr layer is still installed.
        tracing::warn!(provider = name, "telemetry flush abandoned at shutdown");
        ProviderOutcome::Abandoned
    };
    let started_with = |which: Which| pending.iter().any(|(started, _)| *started == which);
    ShutdownReport {
        traces: outcome(Which::Traces, "traces", started_with(Which::Traces)),
        logs: outcome(Which::Logs, "logs", started_with(Which::Logs)),
    }
}

/// The configuration and, for `serve`, the telemetry [`start_up`] produced.
#[derive(Debug)]
pub struct StartUp {
    /// The loaded and validated configuration.
    pub config: Config,
    /// Telemetry, for `serve` only.
    pub telemetry: Option<Telemetry>,
}

/// Loads the configuration and, for `serve`, starts telemetry, under a
/// temporary stderr subscriber.
///
/// The subscriber is a thread-local default (`set_default`), scoped to this
/// function: its guard is dropped before the caller calls [`install`].
/// `set_global_default` does not override a thread-local default that is
/// still active, so a kept guard would leave the start-up thread (and every
/// future polled on it) on the fallback, without OTLP.
///
/// # Arguments
/// * `path` - the `--config` file, if any.
/// * `serve` - whether to start telemetry.
/// * `env` - reads an environment variable.
/// * `writer` - where the temporary fallback subscriber writes (stderr).
///
/// # Errors
/// [`OkfError::Config`] from [`Config::load`] or [`Telemetry::start`].
pub fn start_up<W>(
    path: Option<&Path>,
    serve: bool,
    env: &dyn Fn(&str) -> Option<String>,
    writer: W,
) -> Result<StartUp, OkfError>
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let rust_log = env("RUST_LOG");
    let _startup =
        tracing::subscriber::set_default(subscriber::fallback(rust_log.as_deref(), writer));
    let config = Config::load(path)?;
    let telemetry = serve
        .then(|| Telemetry::start(&config.telemetry, env))
        .transpose()?;
    Ok(StartUp { config, telemetry })
    // `_startup` drops here.
}

/// Installs the process's subscriber: stderr alone, or with the OTLP layers
/// of `telemetry`.
///
/// # Arguments
/// * `config` - `[telemetry]`.
/// * `telemetry` - `serve`'s running telemetry, or `None` for every other
///   command.
/// * `rust_log` - the value of `RUST_LOG`, if set.
///
/// # Errors
/// [`OkfError::Config`] when a configured filter does not parse or a
/// subscriber is already installed.
pub fn install(
    config: &TelemetryConfig,
    telemetry: Option<&Telemetry>,
    rust_log: Option<&str>,
) -> Result<(), OkfError> {
    install_with_writer(config, telemetry, rust_log, std::io::stderr)
}

/// [`install`] with stderr's writer as a parameter, for tests.
pub fn install_with_writer<W>(
    config: &TelemetryConfig,
    telemetry: Option<&Telemetry>,
    rust_log: Option<&str>,
    writer: W,
) -> Result<(), OkfError>
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let error = |message: String| OkfError::Config {
        path: "[telemetry]".to_string(),
        message,
    };
    let settings = subscriber::settings(config, rust_log).map_err(error)?;
    let (tracer, logger) = match telemetry {
        Some(telemetry) => (telemetry.tracer.as_ref(), telemetry.logger.as_ref()),
        None => (None, None),
    };
    let subscriber = subscriber::build_with_writer(settings, tracer, logger, writer);
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|_| error("a tracing subscriber is already installed".to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use async_trait_free::HangingSpanExporter;

    use super::*;

    /// A span exporter whose `export` never completes.
    mod async_trait_free {
        use opentelemetry_sdk::error::OTelSdkResult;
        use opentelemetry_sdk::trace::{SpanData, SpanExporter};

        #[derive(Debug)]
        pub struct HangingSpanExporter;

        impl SpanExporter for HangingSpanExporter {
            async fn export(&self, _batch: Vec<SpanData>) -> OTelSdkResult {
                std::future::pending().await
            }
        }
    }

    #[test]
    fn start_without_otlp_builds_no_providers() {
        let telemetry = Telemetry::start(&TelemetryConfig::default(), &|_| None).expect("starts");
        assert!(!telemetry.traces_enabled());
        assert!(!telemetry.logs_enabled());
    }

    #[test]
    fn start_with_unset_headers_env_names_the_variable() {
        let mut config = TelemetryConfig::default();
        config.otlp = Some(config::OtlpConfig {
            endpoint: Some("https://otlp.example.com".to_string()),
            headers_env: Some("OTLP_HEADERS_FOR_TEST".to_string()),
            ..config::OtlpConfig::default()
        });
        let error = Telemetry::start(&config, &|_| None).expect_err("refused");
        assert!(
            error.to_string().contains("OTLP_HEADERS_FOR_TEST"),
            "{error}"
        );
        let error = Telemetry::start(&config, &|_| Some("no-equals-secret".to_string()))
            .expect_err("refused");
        assert!(
            error.to_string().contains("OTLP_HEADERS_FOR_TEST"),
            "{error}"
        );
        assert!(!error.to_string().contains("no-equals-secret"), "{error}");
    }

    #[test]
    fn otel_sdk_disabled_needs_no_credentials() {
        let mut config = TelemetryConfig::default();
        config.otlp = Some(config::OtlpConfig {
            endpoint: Some("https://otlp.example.com".to_string()),
            headers_env: Some("OTLP_HEADERS_FOR_TEST".to_string()),
            ..config::OtlpConfig::default()
        });
        // The headers variable is unset, and `OTEL_SDK_DISABLED` is the only
        // variable the callback answers.
        let only_disabled = |name: &str| (name == "OTEL_SDK_DISABLED").then(|| "true".to_string());
        let telemetry =
            Telemetry::start(&config, &only_disabled).expect("starts without credentials");
        assert!(!telemetry.traces_enabled() && !telemetry.logs_enabled());
        let dsn = TelemetryConfig {
            otlp: Some(config::OtlpConfig {
                sentry_dsn_env: Some("DSN_FOR_TEST".to_string()),
                ..config::OtlpConfig::default()
            }),
            ..TelemetryConfig::default()
        };
        Telemetry::start(&dsn, &only_disabled).expect("starts without a DSN");
    }

    #[tokio::test]
    async fn otel_sdk_disabled_turns_otlp_off() {
        let mut config = TelemetryConfig::default();
        config.otlp = Some(config::OtlpConfig {
            sentry_dsn_env: Some("DSN".to_string()),
            ..config::OtlpConfig::default()
        });
        let env: HashMap<&str, &str> = HashMap::from([
            ("DSN", "https://public@sentry.example.com/42"),
            ("OTEL_SDK_DISABLED", "TRUE"),
        ]);
        let telemetry = Telemetry::start(&config, &|name| env.get(name).map(ToString::to_string))
            .expect("starts");
        assert!(!telemetry.traces_enabled() && !telemetry.logs_enabled());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_returns_within_the_deadline_with_a_hung_exporter() {
        let processor = BatchSpanProcessor::builder(HangingSpanExporter, runtime::Tokio).build();
        let tracer = SdkTracerProvider::builder()
            .with_span_processor(processor)
            .build();
        // Put one span in the queue so the flush has something to hang on.
        {
            use opentelemetry::trace::{Tracer, TracerProvider};
            let _ = tracer.tracer("test").start("hung");
        }
        let telemetry = Telemetry::from_providers(Some(tracer), None);
        let started = Instant::now();
        let report = tokio::time::timeout(
            SHUTDOWN_DEADLINE + Duration::from_secs(1),
            telemetry.shutdown(),
        )
        .await
        .expect("shutdown returns within the deadline plus a second");
        assert!(started.elapsed() >= SHUTDOWN_DEADLINE.saturating_sub(Duration::from_millis(200)));
        assert_eq!(report.traces, ProviderOutcome::Abandoned);
        assert_eq!(report.logs, ProviderOutcome::NotRunning);
        assert!(!report.all_flushed());
    }

    #[tokio::test]
    async fn second_shutdown_is_quiet() {
        let tracer = SdkTracerProvider::builder().build();
        let logger = SdkLoggerProvider::builder().build();
        // Someone else already shut them down: AlreadyShutdown is not a failure.
        tracer.shutdown().expect("first");
        logger.shutdown().expect("first");
        let report = Telemetry::from_providers(Some(tracer), Some(logger))
            .shutdown()
            .await;
        assert!(report.all_flushed(), "{report:?}");
        assert_eq!(report.traces, ProviderOutcome::Flushed);
    }

    /// Accepts one connection per request, answers `200`, and sends each
    /// request's head (request line and headers) down the channel.
    async fn receiver() -> (u16, tokio::sync::mpsc::UnboundedReceiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let (sender, heads) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let sender = sender.clone();
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0_u8; 8192];
                    // Read until the head is complete; the body is not needed.
                    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                        }
                    }
                    let _ = sender.send(String::from_utf8_lossy(&buffer).into_owned());
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                        )
                        .await;
                });
            }
        });
        (port, heads)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spans_and_logs_reach_the_receiver_with_their_headers() {
        let (port, mut heads) = receiver().await;
        let mut config = TelemetryConfig::default();
        config.otlp = Some(config::OtlpConfig {
            endpoint: Some(format!("http://127.0.0.1:{port}")),
            headers_env: Some("H".to_string()),
            ..config::OtlpConfig::default()
        });
        let telemetry = Telemetry::start(&config, &|name| {
            (name == "H").then(|| "x-test-auth=token-1".to_string())
        })
        .expect("starts");
        assert!(telemetry.traces_enabled() && telemetry.logs_enabled());
        let settings = subscriber::settings(&config, Some("error")).expect("settings");
        let dispatch = telemetry.subscriber(settings);
        tracing::subscriber::with_default(dispatch, || {
            let span = tracing::info_span!(target: "okf_mcp::test", "exported");
            let _entered = span.enter();
            tracing::info!(target: "okf_mcp::test", "exported-log");
        });
        let report = telemetry.shutdown().await;
        assert!(report.all_flushed(), "{report:?}");
        let mut paths = Vec::new();
        while let Ok(Some(head)) = tokio::time::timeout(Duration::from_secs(5), heads.recv()).await
        {
            assert!(
                head.to_ascii_lowercase().contains("x-test-auth: token-1"),
                "{head}"
            );
            paths.push(head.lines().next().unwrap_or_default().to_string());
            if paths.len() == 2 {
                break;
            }
        }
        paths.sort();
        assert!(paths[0].starts_with("POST /v1/logs "), "{paths:?}");
        assert!(paths[1].starts_with("POST /v1/traces "), "{paths:?}");
    }

    #[tokio::test]
    async fn shutdown_without_providers_reports_not_running() {
        let report = Telemetry::from_providers(None, None).shutdown().await;
        assert_eq!(report.traces, ProviderOutcome::NotRunning);
        assert_eq!(report.logs, ProviderOutcome::NotRunning);
        assert!(report.all_flushed());
    }
}

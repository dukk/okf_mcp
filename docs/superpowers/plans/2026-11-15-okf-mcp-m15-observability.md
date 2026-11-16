# OKF MCP Server, Milestone 15 (observability): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`); a lettered task (9a, 9b, ...) is its own sub-issue and PR.

> **Builds on milestone 14 (operator API, epic #390), which has merged**, and on `main` at `bc32c72` (v0.16.0 plus the SES mail provider, PR #457).

**Goal:** The server reports what it is doing. Metrics go to a Prometheus `/metrics` endpoint on a listener of their own and are pushed over OTLP. Traces and logs go over OTLP: one trace per HTTP request, MCP tool call, poll tick and mail job, with every function in the crate able to appear as a span. Logs are correlated with traces and also written to stderr as text or JSON, and stderr and OTLP are filtered independently. Sentry is reached through its OTLP endpoint. A dylint policy lint keeps every function instrumented from now on.

**Architecture:**
- **One `src/telemetry/` module** owns the subscriber, the three OTel providers, the Prometheus registry, the `/metrics` listener, propagation helpers and the typed `Metrics` handle. Everything else in the crate uses `tracing` macros, `#[instrument]` and `Metrics` methods only.
- **One subscriber, a filter per layer.** The layers are stderr (text, or JSON with `trace_id`/`span_id`), OTLP spans (`tracing-opentelemetry`) and OTLP logs (`opentelemetry-appender-tracing`). Nothing filters the whole stack. Each OTLP filter is ANDed with a fixed cap: `rmcp` at `info` at most, and the export stack (`opentelemetry*`, `hyper`, `hyper_util`, `reqwest`, `h2`) off. The stderr filter is capped for `rmcp` only.
- **One `SdkMeterProvider`, two readers.** A `PeriodicReader` pushes OTLP when OTLP is configured. The `opentelemetry-prometheus` reader renders `/metrics`; it is always present for `serve`, so the families exist even with OTLP off.
- **`Metrics` lives on `App`.** It is built from a `Meter`. `App::load` uses the global meter provider, which `serve` sets before `App::load`. Tests call `App::load_with_metrics` with a local provider.
- **Telemetry starts only for `serve`.** The other subcommands get the stderr layer alone. `okf-maintenance` keeps its own fixed subscriber.

**Tech Stack:** Rust 2021, the existing axum 0.8 / sqlx 0.9 / rmcp 3.8 / tokio stack. New dependencies, all pinned together at the release `opentelemetry-prometheus` supports:
- `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `opentelemetry-http`, `opentelemetry-appender-tracing` and `opentelemetry-prometheus`, all at 0.33;
- `tracing-opentelemetry` 0.34;
- `prometheus` 0.14 (no default features).

The `testing` feature of `opentelemetry_sdk` is enabled for dev only. The policy lint is a dylint library, not a Cargo dependency.

**Spec:** `docs/superpowers/specs/2026-11-15-okf-observability-design.md`.

---

## Conventions for every task

- **Branches and PRs.** Branch per task from `main`, one PR per task, linked to its sub-issue. Follow `AGENTS.md` (review rounds, keeping branches current, `tools/agentic/`) and `tools/agentic/brief-common.md`.
- **TDD.** Write the failing test, run it, implement, run it, commit.
- **Gates** run only through `tools/agentic/gate.sh`.
  - `fmt`, `lint` and `test` before every commit.
  - `test-ui` for any task that changes the console (none in this milestone is expected to).
  - `dylint` from Task 8 on, for every task that adds functions. It warns until Task 10 and must be clean after.
  - While iterating, use `gate.sh narrow <cargo test args>`. It takes **one** test-name filter per run.
- **Code style.** Comments explain Rust-specific constructs for a Python-expert, Rust-novice reader, as in the existing code.
  - Every public item gets a doc comment with `# Arguments`, `# Returns` and `# Errors`.
  - No `.unwrap()` outside tests.
  - New errors are `thiserror` variants or `OkfError::Config` messages.
- **Changelog.** One entry per task, `changelog/<UTC ts>-m15-task-<NN>-<slug>.md`, with `## Added`, `## Changed` and `## Notes`.
  - Take the timestamp from `date -u +%Y-%m-%dT%H-%M-%SZ`.
  - `<NN>` is `01`, `09a`, and so on.
  - Task 11 also writes the milestone changelog.
- **The code in this plan has not been compiled.** If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol. The exception is the OTel snippets in Tasks 1 and 2: they come from a probe crate that compiled and ran against the pinned versions (see "Verified facts"). Only the glue around them is uncompiled.
- **Privacy rules.** Each is a review checklist item for every task.
  - Metric attributes come only from closed sets.
  - Span attributes may carry `enduser.id` (the user's id) and `okf.key.id` (the credential's id), never names, email addresses, paths, document text, SQL, URIs, queries, headers, tokens or error text.
  - Every `#[instrument]` has `skip_all`; values go only through `fields(...)`; never `err` or `ret`.
- **Integer span fields are recorded as `i64`.** Other integer types export as strings (Verified facts).
- **Placeholders only** in tests and docs: `example.com`, `otlp.example.com`, `alice`. No real hostnames, domains, names or keys. A Sentry DSN in a test is `https://public@sentry.example.com/42`.
- **Commit messages** end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Verified facts the code relies on

### OpenTelemetry 0.33

These were checked by compiling and running a probe crate on 2026-11-15, with every `opentelemetry*` crate at 0.33.1, tracing-opentelemetry 0.34.0, tracing-subscriber 0.3.23, prometheus 0.14.0 and reqwest 0.13.5. `R` below is `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f`.

**Dependency resolution**
- One `opentelemetry_sdk` 0.33.1 and one `reqwest` 0.13.5 resolve, the same reqwest okf-mcp already uses.
- `opentelemetry-http`'s `reqwest` feature implements `HttpClient` for the async `reqwest::Client` (`R/opentelemetry-http-0.33.1/src/lib.rs:187`). `opentelemetry-otlp`'s `reqwest-client` feature turns it on.
- No `tonic`, no `aws-lc-rs`.

**Resource**
- `Resource::builder()` runs the SdkProvided, Telemetry and Env detectors, so it reads `OTEL_RESOURCE_ATTRIBUTES` and `OTEL_SERVICE_NAME` (`R/opentelemetry_sdk-0.33.1/src/resource/mod.rs:62-67`, `resource/env.rs:9-10`).
- Each later `with_*` call overrides. So `.with_service_name(..)` and `.with_attributes(..)` set in code beat the environment, while env-only keys (`k8s.pod.name`) are kept (`mod.rs:292-305`; ran).

**OTLP exporters**
- Built with `SpanExporter::builder().with_http().with_protocol(Protocol::HttpBinary).with_endpoint(url).with_headers(HashMap<String,String>).with_http_client(reqwest::Client).with_timeout(d).build()`. `LogExporter` and `MetricExporter` are the same; `MetricExporter` also takes `.with_temporality(Temporality::Cumulative)`.
- Needs `use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};` (`R/opentelemetry-otlp-0.33.1/src/exporter/http/mod.rs:834-844`).
- An endpoint given in code is used **verbatim**, so the code appends `/v1/traces` etc. itself (`http/mod.rs:767-774`).
- Code beats env for endpoint, protocol, timeout and compression.
- **Env headers are merged on top of code headers.** An `OTEL_EXPORTER_OTLP_HEADERS` key replaces the same key set in code (`http/mod.rs:294-297`; ran).
- Default retry: 3 retries, 100 ms to 1.6 s. Default user agent: `OTel-OTLP-Exporter-Rust/0.33.1`.

**Batch processors and readers on tokio**
- Spans: `opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor::builder(exporter, runtime::Tokio).build()` (`trace/span_processor_with_async_runtime.rs:409`).
- Logs: `opentelemetry_sdk::logs::log_processor_with_async_runtime::BatchLogProcessor::builder(exporter, runtime::Tokio).build()` (`logs/log_processor_with_async_runtime.rs:217`).
- Metrics: `opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader::builder(exporter, runtime::Tokio).with_interval(d).build()` (`metrics/periodic_reader_with_async_runtime.rs:85`). `with_interval` beats `OTEL_METRIC_EXPORT_INTERVAL`.
- The batch processors still honour `OTEL_BSP_*` and `OTEL_BLRP_*`.
- Features: `rt-tokio`, `experimental_trace_batch_span_processor_with_async_runtime`, `experimental_logs_batch_log_processor_with_async_runtime` and `experimental_metrics_periodicreader_with_async_runtime`.

**Sampler**
- `Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(r)))` (`trace/sampler.rs:150,156`).

**Prometheus reader**
- Built with `opentelemetry_prometheus::exporter().with_registry(registry.clone()).without_target_info().scope_info_enabled(false).build()?` and passed with `SdkMeterProvider::builder().with_reader(..)` (`metrics/meter_provider.rs:287`).
- There is **no** `without_scope_info`; it is `scope_info_enabled(false)` (`R/opentelemetry-prometheus-0.33.1/src/config.rs:78-79`).
- The crate enables `opentelemetry_sdk/experimental_metrics_custom_reader` itself (its `Cargo.toml:76-81`).
- Rendered with `prometheus::TextEncoder::new().encode(&registry.gather(), &mut buf)`.

**Prometheus rendering (ran)**
- `okf.mcp.tool.duration` with unit `s` becomes `okf_mcp_tool_duration_seconds` (`_bucket`, `_sum`, `_count`).
- A `u64_counter` gains `_total`. An `i64_up_down_counter` renders as a gauge.
- A unit `By` appends `_bytes`, so a counter named `x.bytes` with unit `By` becomes `x_bytes_bytes_total`. Name byte counters `x.size`.
- No `target_info`, no `otel_scope_*` labels.
- `# HELP` appears only when `.with_description(..)` is set.
- **Histogram boundaries must be given** with `.with_boundaries(vec![..])`; the defaults are scaled for milliseconds.

**Instruments**
- `meter.f64_histogram(name).with_unit("s").with_description(..).with_boundaries(..).build()`;
- `u64_counter(name).build()`;
- `i64_up_down_counter(name).build()`;
- `u64_observable_gauge(name).with_callback(|observer| observer.observe(v, &attrs)).build()`. `i64_observable_gauge` and `f64_observable_gauge` exist alike.

**Subscriber layers**
- **`EnvFilter` is `Clone`** in 0.3.23. The clone copies directives but not dynamic state (`R/tracing-subscriber-0.3.23/src/filter/env/mod.rs:213`).
- `Option<L>` is a `Layer`. `Registry::default().with(a).with(b)...` with `Option` layers compiled.
- Span layer: `tracing_opentelemetry::layer().with_tracer(provider.tracer("okf-mcp"))`.
  - Options: `.with_error_records_to_exceptions(bool)`, `.with_error_events_to_status(bool)`, `.with_location(bool)`, `.with_threads(bool)`, `.with_tracked_inactivity(bool)` and `.with_target(bool)`, which defaults to true and adds a `target` attribute (`R/tracing-opentelemetry-0.34.0/src/layer.rs:104, 667, 717, 757-895`).
- Log bridge: `opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&logger_provider)` (`R/opentelemetry-appender-tracing-0.33.1/src/layer.rs:383`).

**JSON with trace ids**
- The built-in JSON formatter has no trace id (`fmt/format/json.rs:100-111`). `OtelData` is private in 0.34; the public path is `tracing_opentelemetry::get_otel_context(&span_id, &Dispatch)` (`lib.rs:134`).
- `tracing::dispatcher::get_default` inside a `FormatEvent` returns `NONE` under a scoped dispatcher (`R/tracing-core-0.1.36/src/dispatcher.rs:383-397`). So the dispatch is captured by a no-op layer's `on_register_dispatch` into an `Arc<OnceLock<WeakDispatch>>` (ran). Output: `{"level":"INFO","message":"...","span_id":"...","target":"...","trace_id":"..."}`.

**Special span fields**
- `otel.name`, `otel.kind`, `otel.status_code` (`ok`/`error`, case-insensitive) and `otel.status_description` (which forces Error) (`layer.rs:30-33, 199-205, 536-566`).
- The attribute visitor implements only `bool`, `f64`, `i64`, `str` and `debug` (`layer.rs:516-530`). A `u16`/`u64` field exports as a **String** (ran: `status_u16 = String("200")`, `status_i64 = I64(200)`).

**`OpenTelemetrySpanExt`** (`R/tracing-opentelemetry-0.34.0/src/span_ext.rs:60, 98, 104, 133`)
- `set_parent(&self, Context) -> Result<(), SetParentError>`;
- `add_link(&self, SpanContext)`;
- `add_link_with_attributes(&self, SpanContext, Vec<KeyValue>)`;
- `context(&self) -> Context`.

**Propagation**
- Extract: `TraceContextPropagator::new().extract(&opentelemetry_http::HeaderExtractor(&headers))`, then `cx.span().span_context().clone()` (`TraceContextExt`), checking `.is_valid()`.
- Inject: `inject_context(&span.context(), &mut opentelemetry_http::HeaderInjector(&mut headers))` (`R/opentelemetry-http-0.33.1/src/lib.rs:77, 99`). Both work on an http 1.x `HeaderMap`.

**Shutdown**
- `force_flush()`, `shutdown()` and `shutdown_with_timeout(d)` return `OTelSdkResult = Result<(), OTelSdkError>`. The variants are `AlreadyShutdown`, `Timeout(Duration)` and `InternalFailure(String)` (`R/opentelemetry_sdk-0.33.1/src/error.rs:13`).
- A second `shutdown` gives `AlreadyShutdown`. An export failure during shutdown shows as `InternalFailure`.
- All three providers are `Clone + Send + Sync + 'static`.

**Exporter traits**
- `SpanExporter::export(&self, Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send`, plus `shutdown_with_timeout`, `force_flush` and `set_resource` (`trace/export.rs:17`).
- `LogExporter::export(&self, LogBatch<'_>)` (`logs/export.rs:124`).
- `PushMetricExporter::export(&self, &ResourceMetrics)`, `force_flush`, `shutdown_with_timeout` and `temporality` (`metrics/exporter.rs:13`).
- A wrapper with `async fn export` compiles (ran).

**`internal-logs` targets**
- They are each crate's `CARGO_PKG_NAME`: `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `opentelemetry-http`, `opentelemetry-prometheus` and `opentelemetry-appender-tracing`, some hyphenated (`R/opentelemetry-0.33.1/src/global/internal_logging.rs:29, 80, 137, 188`).
- An `EnvFilter` with hyphenated targets parses.

**Global setters** (all return `()`)
- `opentelemetry::global::set_tracer_provider(p)`, `set_meter_provider(p)` and `set_text_map_propagator(p)` (`global/trace.rs:427`, `global/metrics.rs:21`, `global/propagation.rs:25`).

**Test exporters**
- `opentelemetry_sdk::trace::InMemorySpanExporter`, `logs::InMemoryLogExporter` and `metrics::InMemoryMetricExporter`, behind the `testing` feature (`trace/mod.rs:29-34`).
- Read back with `get_finished_spans()`, `get_emitted_logs()` and `get_finished_metrics()`. Wire them with `builder().with_simple_exporter(x)`.

### rmcp 3.8.0

- `#[tool_handler]` writes `call_tool` **only if the impl block has none** (`R/rmcp-macros-3.8.0/src/tool_handler.rs:44-55`). Its body is:

  ```rust
  async fn call_tool(&self, request: rmcp::model::CallToolRequestParams,
      context: rmcp::service::RequestContext<rmcp::RoleServer>)
      -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
      let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
      self.tool_router.call(tcc).await
  }
  ```

  So writing our own `call_tool` in `impl ServerHandler for OkfServer` gives one choke point for every tool.
- `CallToolRequestParams.name: Cow<'static, str>` and `.arguments: Option<JsonObject>` (`R/rmcp-3.8.0/src/model.rs:4409-4417`).
- `CallToolResponse` is an enum: `Complete(CallToolResult)`, `InputRequired(..)` or `Task(..)` (`model/mrtr.rs:105-113`). `CallToolResult.is_error: Option<bool>` (`model.rs:4158`).

### okf-mcp on `bc32c72`

**Startup**
- `src/main.rs:13` sets `DEFAULT_LOG_FILTER = "warn,okf_mcp=info,tower_http=info"`. `main` (`:22-33`) installs `fmt().with_env_filter(..).with_writer(stderr).init()` **before** `cli::run`, for every subcommand.
- `cli::run(cli, clock)` (`src/cli.rs:726`) runs `Config::load` (`:728`) for every command.
- The `Command::Serve` arm (`:734-755`) runs `connect` (`:735`), the `--listen` override (`:738`), `App::load(pool, config, clock)` (`:742`), `TcpListener::bind` (`:745`), `info!("serving")` (`:748`), `CancellationToken::new()` (`:749`), spawns `cancel_on_signal` (`:751`, defined `:877`), then `http::serve(app, listener, shutdown)` (`:752`). Every other command is its own arm (`:756-868`).
- `src/bin/okf-maintenance.rs:247-258` has its own `quiet_logging` subscriber (unchanged).

**Config**
- `Config` (`src/config.rs:492-548`) is `#[serde(default)]` with **no** `deny_unknown_fields` (unknown keys are ignored on purpose, `:489-491`). Tables are non-`Option` structs with their own `Default` (`MailConfig` `:379-396`, whose subtables are `Option<..>`).
- `OidcProviderConfig` (`:145-148`) uses `deny_unknown_fields`.
- Durations use `#[serde(with = "humantime_serde")]`.
- `validate(&self) -> Result<(), String>` (`:637`) ends by chaining `self.auth.validate()?; self.mail.validate()?; self.validate_oidc()?; self.validate_public_url()` (`:695-698`). `load` (`:830`) wraps the string in `OkfError::Config { path, message }`. `from_toml` is at `:615`.
- Secrets are named env vars (`password_env`, `token_env`, `client_secret_env`), read through an injectable `&dyn Fn(&str) -> Option<String>` (`src/mail/mod.rs:96, 104, 152`). That function refuses an unset or blank value, naming the variable and never the value. `secrecy` is **not** a dependency.
- Tests build `Config { .., ..Config::default() }` or `Config::from_toml`.

**Serving: `http::serve(app, listener, shutdown)` (`src/http/mod.rs:363`)** runs in this order:
1. `router` (`:368`);
2. spawns `poll_until` (`:369`) and `check_clock_until` (`:370`);
3. the mail worker if `mail.worker` (`:373-377`);
4. `axum::serve(..).with_graceful_shutdown(..)` (`:383-388`);
5. `shutdown.cancel()` (`:391`);
6. awaits the three tasks (`:393-404`);
7. `settle_access` with a 30 s limit (`:407`).

**Router and auth middleware**
- `router(app, &shutdown)` is at `:241`. `TraceLayer::new_for_http().make_span_with(request_span).on_response(DefaultOnResponse::new().level(Level::INFO))` is at `:307-311`.
- `request_span` (`:331-344`) is `info_span!("request", method = %request.method(), path = %route, version = ?request.version())`, with `UNMATCHED_ROUTE = "<unmatched>"` (`:318`).
- `mcp_service` (`:212-231`) uses `LocalSessionManager::default()` and a factory closure that runs once per session (`:227`).
- The principal is inserted as `Arc<Principal>` by `auth::require_key` (`src/auth.rs:1174`, insert at `:1192`) and by `console_session::require_principal` (`src/http/console_session.rs:356`, inserts at `:371` and `:403`).
- `Principal` has `key_id: String` and `user_id: Option<String>` among its fields, and **no team id** (`src/auth.rs:163-199`).

**`App`** (`src/app.rs:72-133`)
- The only constructor is `App::load(pool, config, clock) -> Result<Arc<Self>, OkfError>` (`:158`).
- `pub type Catalogs = BTreeMap<String, Arc<CatalogState>>` (`:38`). Accessors: `catalogs()` (`:407`), `catalog(name)` (`:418`), `pool()` (`:277`).
- `poll` (`:447-501`) calls `reload_keys_if_changed` (`:510`, `keys.reload` at `:526`). `poll_until` is at `:652-671`, `check_clock_until` at `:678-701`, `note_failure` at `:740`.
- Per catalog: `CatalogState::current() -> Arc<Loaded>` (`src/catalog.rs:220`), `Loaded::generation() -> i64` (`:43`), `Loaded::snapshot()` (`:48`), `Snapshot::concepts()` (`src/okf/snapshot.rs:317`), `CatalogState::publish_lock()` (`src/catalog.rs:231`).
- Test helpers: `mcp::testing::{TestApp, app_with, app_at, app_with_new}` (`src/mcp/testing.rs:25-108`), `http::api::testing::{app, login_app}` (`src/http/api/testing.rs:40, 283`).

**MCP**
- `OkfServer { app, tool_router }` (`src/mcp/mod.rs:47`) and `tools()` (`:71-85`, 13 routers joined with `+`).
- `#[tool_handler(router = self.tool_router)] impl ServerHandler for OkfServer` writes only `get_info` by hand (`:108-120`).
- `tool_error` (`:137`) logs and returns `CallToolResult::structured_error(error.to_json())`. `OkfError::kind() -> &'static str` is at `src/error.rs:319`.
- **There are 42 tools**, in `src/mcp/annotations.rs`: `READS` (17, `:13`), `STAGED` (21, `:35`) and `DESTRUCTIVE` (4, `:61`).

**Publish** (`src/store/publish.rs:129`): `publish(app, state, input, audit) -> Result<PublishReport, OkfError>`.
- It takes `state.publish_lock().lock().await` (`:150`), then the advisory lock (`:154`).
- A lint failure is `Err(OkfError::LintFailed { .. })` from `check` (`:396`, `:419`).

**Mail** (`src/mail/queue.rs`)
- `spawn_worker(app, shutdown)` is at `:504`.
- The handler is `send_mail(job, context, id)` (`:398`); `job.template` is available. Its outcomes are `Ok` (logs "mail sent"), `Permanent` and `Retryable`.
- Providers are `mail::{log, smtp, postmark, ses}`.

**reqwest clients**
- OIDC `src/oidc.rs:276`, CIMD `src/oauth/clients.rs:635`, Postmark `src/mail/postmark.rs:80` and the investigator `src/investigator/client.rs:80`.
- SES uses the AWS SDK.

**Sign-in**
- Success goes through `signin::start_in` (`src/http/signin.rs:469`) and refusal through `signin::audit(app, user_id, method, outcome, ip)` (`:522`).
- Both reach `store::sign_in_events::record`/`record_in` (`:141`, `:170`), with `Method` Password/Passkey/Oidc/Setup (`:15`) and `Outcome` Ok/Failed/Locked (`:28`).

**OAuth token endpoint and rate limiters**
- The token endpoint is `oauth/token.rs`: `exchange_code` (`:276`) and `refresh` (`:370`).
- `TokenBucket::try_take` is at `src/oauth/ratelimit.rs:43` (its refusals are at token.rs `:260` and `:431`, authorize.rs `:680` and clients.rs `:880`).
- `KeyedLimiter::try_take` is at `src/http/limits.rs:76`, and `SignInLimits` at `:95`.

**Pool** (`src/db.rs:30`): `connect(url, max_connections)` sets only `max_connections`. `App::pool()` is public, so `pool.size()` and `pool.num_idle()` are reachable.

**Clock** (`src/clock_check.rs`): `ClockStatus { ok, checked_at, offset_ms: Option<i64>, stratum }` (`:27`). `check_once` is at `:52`.

**Archives** (`src/http/archives.rs`)
- `export_catalog` is at `:159`; its byte length is known at `:182`.
- `import_catalog` is at `:254`; the body is `Bytes` at `:277`.

**Tests**
- `tests/support/server.rs:494` `serve_app` binds `127.0.0.1:0`, forces `mail.worker = false`, calls `App::load` and spawns `http::serve`.
- The existing log-capture test in `src/http/mod.rs:424-501` installs a **thread-local** subscriber (`set_default`).

**Tooling**
- The justfile has `fmt`, `lint` (clippy `-D warnings` + `cargo doc -D warnings`), `test`, `test-ui` and `test-agentic`, and exports `SQLX_OFFLINE=true`.
- `tools/agentic/gate.sh` maps `fmt|lint|test|test-ui|narrow` to timeouts and the shared lock (`:52-58`).
- There is **no test CI**: `.github/workflows/` holds only `image.yml` and `maintenance-image.yml`.
- `Cargo.toml` has no `[workspace]` or `[lints]` table. Task 8 adds an empty `[workspace]` (with `exclude = [".worktrees"]`) and `[lints.rust.unexpected_cfgs]`.

**Policy lint**
- The `hyp_tracing` library's `uninstrumented_fn` lint is pinned by tag `v0.1.2` in the policy-lints repository (tag object `3dc37e5`, commit `8661186`).
- `cargo-dylint` is installed on the build machine.
- The sibling project's policy doc (rules adapted into Task 8's `docs/contributing/policy-lints.md`) says `const fn`, closures and test-only code are out of scope. The functions rmcp's `#[tool_router]`, `#[tool]` and `#[tool_handler]` generate are not flagged.
- An exemption is `#[cfg_attr(dylint_lib = "hyp_tracing", allow(uninstrumented_fn, reason = "..."))]`, which needs `unexpected_cfgs` `check-cfg = ["cfg(dylint_lib, values(any()))"]`.

## Decisions taken while planning (2026-11-15)

1. **Milestone 15.** M14 (operator API) is closed. Labels: `epic`, `milestone-15`.
2. **Only the `hyp_tracing` lint library** is loaded (`pattern = "lints/hyp_tracing"`), not `hyp_hegel`. That library would flag essentially every existing test and is out of scope.
3. **A new `gate.sh dylint` gate**, wrapping a new `just dylint` recipe, with the shared lock and a 45 m timeout. It is required before push for every task from Task 8 on. The lint warns until Task 10 and denies after. No GitHub CI job (the repository has no test CI).
4. **The plan PR is merged by the coordinator** after review, and the issues are filed after that.
5. **One choke point for MCP tool telemetry.** `OkfServer` writes its own `call_tool` (so `#[tool_handler]` skips generating one) and opens the `mcp <tool>` span and records `okf.mcp.tool.duration` there. The tools' own `#[instrument]` (from the sweep) sit beneath it at `debug`. This replaces the spec's "`#[instrument(level = "info")]` below each `#[tool]`".
6. **`App::load_with_metrics(pool, config, clock, metrics)`** is added, and `App::load` becomes `load_with_metrics(.., Metrics::global())`. Only tests that assert metrics use the new form, so no existing caller changes.
7. **Telemetry tables reject unknown keys** (`deny_unknown_fields` on `[telemetry]` and its subtables), as `[[auth.oidc]]` does. A misspelt `headers_env` would otherwise silently send no auth.
8. **Secrets are `String`s with a hand-written `Debug`**, matching the mail and OIDC code (no `secrecy` dependency). `OtlpTarget`'s `Debug` prints header names only.

## Simplifications and deviations relative to the design

The plan PR applied these to the spec, each marked `(Corrected, M15 plan, 2026-11-15)`.
- **42 MCP tools, not 58.**
- **No `okf.team.id` span attribute.** `Principal` has no team. Spans carry `enduser.id` and `okf.key.id`, plus `okf.catalog` where a catalog is known.
- **Tool spans come from `call_tool`** (Decision 5), not `#[instrument(level = "info")]` on each tool.
- **`okf.archive.bytes` becomes `okf.archive.size`** (unit `By`, rendered `okf_archive_size_bytes_total`); the original name would render `_bytes_bytes_total`.
- **`OTEL_EXPORTER_OTLP_HEADERS` overrides** a header of the same name from `headers_env`. That is the SDK's behaviour, and it is documented, not fought.
- **A `/metrics` scrape answers `text/plain; version=0.0.4`**, the encoding `prometheus::TextEncoder` writes.
- **No `db.client.connection.wait_time`.** sqlx 0.9 has no acquire hook, and timing every `acquire`/`begin` by hand is out of proportion. The pool gauges stay.

---

## Scope

| Task | Delivers |
|---|---|
| 1 | `[telemetry]` config and validation; `telemetry::subscriber` (layers, caps, JSON with trace ids); OTLP traces and logs; the Sentry DSN target; counting exporters; `Telemetry::start`/`shutdown`; `serve` wiring; stderr-only subscriber for the other subcommands; the spec deviations |
| 1b | The privacy audit of every existing `tracing` event: caller-supplied text, full error text and the `log` mail provider move to `LOCAL_ONLY` (stderr only) or are replaced by closed values |
| 2 | `telemetry::metrics::Metrics` (instruments, label enums, buckets); the Prometheus reader; `[telemetry.prometheus] listen`; the `/metrics` listener; `okf.build.info`; export-failure counters on `/metrics`; `App::load_with_metrics` |
| 3 | The HTTP server span (semconv fields, i64 status, error on 5xx, `traceparent` link, `enduser.id`/`okf.key.id`); `http.server.*` metrics; `OkfServer::call_tool` span and `okf.mcp.tool.duration`; `okf.mcp.sessions.active` |
| 4 | Publish, changeset, poll loop and cache spans and metrics |
| 5 | Sign-in, OAuth, rate-limit and mail spans and metrics |
| 6 | Postgres pool gauges, clock check, archives; outbound `traceparent` injection on every reqwest client |
| 7 | The mock OTLP collector and the privacy sentinel test |
| 8 | dylint wiring (warn), `just dylint`, `gate.sh dylint`, `docs/contributing/policy-lints.md` |
| 9a-9e | The `#[instrument]` sweep, by module group |
| 10 | The lint at deny |
| 11 | `docs/operations.md` "Telemetry", Kubernetes and Grafana examples, the dashboard test, README, milestone changelog, smoke test |

**Deliberately deferred**

| Item | Where it goes |
|---|---|
| OTLP export from `okf-maintenance` (needs its own privacy review: rmcp traces there carry evidence and tokens) | follow-up issue, filed by Task 11 |
| The native Sentry SDK (`sentry-tracing` as one more layer) | follow-up issue only if Sentry's OTLP issue grouping proves too weak; not filed now |
| Telemetry for the CLI subcommands | not planned |
| `hyp_hegel` (property-test policy) | not planned |
| A GitHub CI job for dylint | not planned (no test CI exists) |

## File map

| File | Tasks |
|---|---|
| `Cargo.toml` | 1 (otel deps, tracing-subscriber `registry`), 2 (`opentelemetry-prometheus`, `prometheus`), 8 (dylint metadata, `unexpected_cfgs`) |
| `src/main.rs` | 1 |
| `src/lib.rs` | 1 (`pub mod telemetry;`) |
| `src/cli.rs` | 1 (serve arm, subscriber for other arms), 2 (spawn the `/metrics` listener) |
| `src/config.rs` | 1 (`TelemetryConfig`, validation), 2 (`PrometheusConfig`, overlap check) |
| `src/telemetry/mod.rs` | 1 (create), 2 |
| `src/telemetry/config.rs` | 1 (create), 2 |
| `src/telemetry/subscriber.rs` | 1 (create) |
| `src/telemetry/otlp.rs` | 1 (create) |
| `src/telemetry/json.rs` | 1 (create) |
| `src/telemetry/metrics.rs` | 2 (create), 3, 4, 5, 6 (each adds its instruments) |
| `src/telemetry/scrape.rs` | 2 (create) |
| `src/telemetry/propagation.rs` | 3 (create: inbound link), 6 (outbound inject) |
| `src/app.rs` | 2 (`metrics` field, `load_with_metrics`), 4 (poll spans, gauges) |
| `src/http/mod.rs` | 3 (`request_span`) |
| `src/auth.rs`, `src/http/console_session.rs` | 3 (record principal ids) |
| `src/mcp/mod.rs` | 3 (`call_tool`) |
| `src/store/publish.rs`, `src/mcp/changesets.rs` | 4 |
| `src/http/signin.rs`, `src/oauth/token.rs`, `src/oauth/ratelimit.rs`, `src/http/limits.rs`, `src/mail/queue.rs` | 5 |
| `src/db.rs`, `src/clock_check.rs`, `src/http/archives.rs`, `src/oidc.rs`, `src/oauth/clients.rs`, `src/mail/postmark.rs` | 6 |
| `tests/support/otlp_collector.rs`, `tests/telemetry_privacy.rs`, `tests/support/mod.rs` | 7 |
| `justfile`, `tools/agentic/gate.sh`, `tools/agentic/README.md`, `tools/agentic/brief-common.md`, `docs/contributing/policy-lints.md`, `AGENTS.md` | 8 (Task 10 flips the level in `Cargo.toml`/`justfile`) |
| `src/store/**` | 9a |
| `src/okf/**`, `src/overlay.rs`, `src/render.rs`, `src/selection.rs` | 9b |
| `src/http/**`, `src/oauth/**`, `src/oidc.rs`, `src/oidc/**` | 9c |
| `src/mcp/**`, `src/auth.rs`, `src/mail/**`, `src/cli.rs`, `src/cli/**`, `src/app.rs`, `src/catalog.rs`, `src/config.rs`, `src/archive.rs`, `src/db.rs`, `src/clock*.rs`, `src/ntp.rs`, `src/keys.rs`, `src/keyed_lock.rs`, `src/error.rs`, `src/write.rs`, `src/main.rs` | 9d |
| `src/investigator/**`, `src/bin/**`, `src/maintenance/**` | 9e |
| `docs/operations.md`, `examples/kubernetes/*`, `examples/grafana/okf-mcp.json`, `tests/grafana_dashboard.rs`, `README.md`, `changelog/*-m15-milestone.md` | 11 |
| `docs/superpowers/specs/2026-11-15-okf-observability-design.md` | any task that corrects it |
| `changelog/` | every task |

## Task dependencies

Each line is `task (the tasks it needs)`; a task starts when every task in its parentheses has merged. Tasks on one line can run in parallel.

```
wave 1:  1 ()                8 ()
wave 2:  2 (1)   1b (1)       9b (8)      9e (8)
wave 3:  3 (2)  4 (2)  5 (2)  6 (2)
wave 4:  7 (1b, 3, 4, 5, 6)         9a (8, 4, 6)   9c (8, 3, 5, 6)   9d (8, 3, 4, 5)
wave 5:  10 (7, 9a, 9b, 9c, 9d, 9e)
wave 6:  11 (all)
```

Why each edge, and the shared files to merge with care:
- **1 and 8 start together.** 1 owns `src/telemetry/`, `src/main.rs`, `src/cli.rs` and `src/config.rs`; 8 owns the justfile, `gate.sh` and the docs. Both touch `Cargo.toml` (8 adds `[package.metadata.dylint]` and `[lints.rust]`, 1 adds dependencies): keep both.
- **2 needs 1** (`Telemetry`, `TelemetryConfig`, the resource). **1b needs 1** (`LOCAL_ONLY` and the cap). It touches event lines in files that 3-6 also edit, so merge `main` and keep both sides; it runs beside 2.
- **3, 4, 5 and 6 need 2** (the `Metrics` handle; 3 and 6 also share `telemetry/propagation.rs`, which 3 creates with `link_from` and 6 extends with `inject`). They each add methods to `src/telemetry/metrics.rs` in separate `impl` blocks and add instruments to the `Instruments` struct: keep both sides. Task 6 creates `propagation.rs` itself if it merges before Task 3 (*whoever is second keeps both functions*).
- **7 needs 1b, 3, 4, 5 and 6.** Its sentinel test goes through the existing events (1b), the request span (3), publish (4), sign-in (5) and the archive routes (6).
- **9b and 9e need only 8.** Their modules are untouched by 3-6.
- **9a needs 4 and 6** (`store/publish.rs`, and the pool spans in `store/` from 6). **9c needs 3, 5 and 6** (`http/`, `oauth/`, `oidc.rs`). **9d needs 3, 4 and 5** (`mcp/`, `auth.rs`, `app.rs`, `mail/`). This keeps the sweep from conflicting with the hand-written instrumentation.
- **10 needs every sweep task and 7.** It runs the sentinel test over the fully swept tree, because a sweep PR can merge before Task 7 exists (review of PR #459). **11 is last.**

---

### Task 1: Telemetry core: config, subscriber, OTLP traces and logs, shutdown

**Files:**
- Create: `src/telemetry/mod.rs`, `src/telemetry/config.rs`, `src/telemetry/subscriber.rs`, `src/telemetry/otlp.rs`, `src/telemetry/json.rs`
- Modify: `Cargo.toml`, `src/lib.rs`, `src/main.rs`, `src/cli.rs` (serve arm, other arms), `src/config.rs` (`telemetry` field, validate chain)

- [ ] **Step 1: Dependencies**

```toml
# OpenTelemetry: spans and log events over OTLP/HTTP-protobuf (src/telemetry).
# Every opentelemetry* crate is pinned at the release opentelemetry-prometheus
# supports (Task 2): its reader must be the same SDK major as the provider,
# or it is not the trait SdkMeterProvider accepts. Default features off: no
# tonic (a second TLS stack), no blocking client. `reqwest-client` sends with
# the async reqwest 0.13 below, on ring. The batch processors and the periodic
# reader run on tokio (`rt-tokio` and the `experimental_*_with_async_runtime`
# features). `internal-logs`: export failures become `tracing` events with the
# exporting crate's name as target, which the OTLP layers turn off (they reach
# stderr only).
opentelemetry = { version = "0.33.1", default-features = false, features = ["trace", "logs", "metrics", "internal-logs"] }
opentelemetry_sdk = { version = "0.33.1", default-features = false, features = ["trace", "logs", "metrics", "internal-logs", "rt-tokio", "experimental_trace_batch_span_processor_with_async_runtime", "experimental_logs_batch_log_processor_with_async_runtime", "experimental_metrics_periodicreader_with_async_runtime"] }
opentelemetry-otlp = { version = "0.33.1", default-features = false, features = ["http-proto", "reqwest-client", "trace", "logs", "metrics", "internal-logs"] }
# Header extraction and injection for W3C trace context on http 1.x maps.
opentelemetry-http = { version = "0.33.1", default-features = false }
# `tracing` events as OTLP log records.
opentelemetry-appender-tracing = { version = "0.33.1", default-features = false }
# `tracing` spans as OTLP spans. 0.34 is the release built on opentelemetry 0.33.
tracing-opentelemetry = { version = "0.34.0", default-features = false }
```

Also:
- Change `tracing-subscriber`'s features to `["fmt", "env-filter", "registry"]`.
- Add `opentelemetry_sdk = { version = "0.33.1", default-features = false, features = ["testing", "trace", "logs", "metrics"] }` under `[dev-dependencies]`. Features unify, so the test build gets the in-memory exporters.

- [ ] **Step 2: Write the failing config tests** (`src/telemetry/config.rs`, `#[cfg(test)]`)

```rust
#[test]
fn absent_table_means_text_stderr_and_no_otlp() {
    let config = Config::from_toml("").expect("parses");
    assert_eq!(config.telemetry, TelemetryConfig::default());
    assert!(config.telemetry.otlp.is_none());
    assert_eq!(config.telemetry.stderr.format, LogFormat::Text);
}

#[test]
fn endpoint_without_headers_env_is_refused() {
    let config = Config::from_toml(
        "[telemetry.otlp]\nendpoint = \"https://otlp.example.com\"\n",
    ).expect("parses");
    let error = config.validate().expect_err("refused");
    assert!(error.contains("telemetry.otlp.endpoint and headers_env"), "{error}");
}

#[test]
fn sentry_dsn_env_excludes_endpoint() { /* endpoint + headers_env + sentry_dsn_env -> Err naming both */ }

#[test]
fn unknown_key_in_telemetry_is_refused() {
    assert!(Config::from_toml("[telemetry.otlp]\nheader_env = \"X\"\n").is_err());
}

#[test]
fn sample_ratio_out_of_range_is_refused() { /* 1.5 and -0.1 -> Err */ }

#[test]
fn endpoint_with_query_or_non_http_scheme_is_refused() { /* "ftp://..", "https://h?x=1", "https://h#f" */ }

#[test]
fn filters_must_parse() { /* traces_filter = "=[" -> Err naming telemetry.otlp.traces_filter */ }
```

Also a property test over `parse_headers` (the `name=value,name=value` format):
- any list of valid header names and printable values round-trips;
- `none` yields an empty map;
- a pair without `=` is refused, and the error never contains the value.

- [ ] **Step 3: Run them and see them fail**

Run: `tools/agentic/gate.sh narrow --lib -- telemetry::config`. Expected: compile errors (no `TelemetryConfig`).

- [ ] **Step 4: Implement `src/telemetry/config.rs`**

```rust
//! The `[telemetry]` table (spec "Configuration"). Secrets are never here:
//! only the names of the environment variables that hold them, read when
//! `serve` starts (`otlp::OtlpTarget::resolve`).

use std::net::SocketAddr;
use std::time::Duration;

use serde::Deserialize;

/// The service name OTLP reports when `service_name` is unset.
pub const DEFAULT_SERVICE_NAME: &str = "okf-mcp";
/// stderr's filter when neither `RUST_LOG` nor `[telemetry.stderr] filter` is set.
pub const DEFAULT_STDERR_FILTER: &str = "warn,okf_mcp=info,tower_http=info";
/// The OTLP span and log filters' default.
pub const DEFAULT_OTLP_FILTER: &str = "info";
/// How often metrics are pushed over OTLP by default.
pub const DEFAULT_EXPORT_INTERVAL: Duration = Duration::from_secs(60);

/// `[telemetry]`.
// `deny_unknown_fields` (Decision 7): a misspelt key would otherwise
// silently turn a setting off.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetryConfig {
    /// `service.name`.
    pub service_name: String,
    /// `deployment.environment.name`, when set.
    pub environment: Option<String>,
    /// `[telemetry.stderr]`.
    pub stderr: StderrConfig,
    /// `[telemetry.otlp]`; OTLP is off without it.
    pub otlp: Option<OtlpConfig>,
    // Task 2 adds `prometheus: Option<PrometheusConfig>`.
}

/// `[telemetry.stderr]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StderrConfig {
    /// `text` (today's lines) or `json` (one object per line, with trace ids).
    pub format: LogFormat,
    /// An `EnvFilter` directive; `RUST_LOG` overrides it.
    pub filter: Option<String>,
}

/// stderr's line format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable lines (the default).
    #[default]
    Text,
    /// One JSON object per line.
    Json,
}

/// `[telemetry.otlp]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OtlpConfig {
    /// The receiver's base URL; `/v1/traces`, `/v1/logs` and `/v1/metrics`
    /// are appended.
    pub endpoint: Option<String>,
    /// The environment variable holding `name=value,name=value` headers, or `none`.
    pub headers_env: Option<String>,
    /// The environment variable holding a Sentry DSN, instead of the two above.
    pub sentry_dsn_env: Option<String>,
    /// The OTLP span layer's filter.
    pub traces_filter: String,
    /// The OTLP log layer's filter.
    pub logs_filter: String,
    /// The share of new traces sampled (a parent's decision is kept);
    /// `None` leaves `OTEL_TRACES_SAMPLER` (or always-on) in charge.
    pub sample_ratio: Option<f64>,
    /// How often metrics are pushed.
    #[serde(with = "humantime_serde")]
    pub export_interval: Duration,
}
```

Notes for the rest of the file:
- Write `Default` impls with the `DEFAULT_*` constants.
- Write `impl TelemetryConfig { pub fn validate(&self) -> Result<(), String> }`, which checks, in order:
  - `endpoint` and `headers_env` set together;
  - `sentry_dsn_env` exclusive of both;
  - with neither, refuse an `[telemetry.otlp]` table that sets nothing (`"telemetry.otlp needs endpoint and headers_env, or sentry_dsn_env"`);
  - the endpoint's scheme is `http`/`https`, with no query, no fragment and a host present (use `reqwest::Url::parse`);
  - `sample_ratio` in `0.0..=1.0` and finite;
  - `export_interval` above zero;
  - each filter parses: `EnvFilter::try_new(..)` on `[telemetry.stderr] filter`, `traces_filter` and `logs_filter`, with the error naming the key.
- `Config` gains `pub telemetry: TelemetryConfig` (`#[serde(default)]` on `Config` already covers it). `Config::validate` chains `self.telemetry.validate()?;` before `self.validate_public_url()`.

- [ ] **Step 5: `src/telemetry/otlp.rs`, the target and the exporters**

```rust
/// Where OTLP goes and with which headers, resolved from the config and the
/// environment at start-up.
// A hand-written `Debug` (Decision 8): header values carry credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct OtlpTarget {
    /// The base URL, without a trailing slash.
    base: String,
    /// Header name -> value.
    headers: HashMap<String, String>,
}

impl std::fmt::Debug for OtlpTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Only the header names; the base URL with userinfo, query and
        // fragment already refused by validation.
        let mut names: Vec<&str> = self.headers.keys().map(String::as_str).collect();
        names.sort_unstable();
        f.debug_struct("OtlpTarget").field("base", &self.base).field("headers", &names).finish()
    }
}
```

The rest of `otlp.rs`:
- **`OtlpTarget::resolve(config: &OtlpConfig, env: &dyn Fn(&str) -> Option<String>) -> Result<Self, OkfError>`.** With `headers_env`, it reads the variable through the same rules as `mail::secret` (unset or blank is `OkfError::Config` naming the variable), then `parse_headers`. With `sentry_dsn_env`, it calls `from_sentry_dsn`.
- **`from_sentry_dsn(dsn: &str) -> Result<OtlpTarget, String>`:**
  - parse the URL;
  - the public key is the username (refuse an empty one);
  - the project id is the last path segment (digits only);
  - `base = format!("{scheme}://{host}{port}{prefix}api/{project}/integration/otlp")`, where `{port}` is `:N` only when the URL has one, and `{prefix}` is the path before the project id, which ends in `/`;
  - `headers = {"x-sentry-auth": format!("sentry sentry_key={key}")}`.
  - Test: `https://public@sentry.example.com/42` gives `https://sentry.example.com/api/42/integration/otlp` and `sentry sentry_key=public`. Also test a DSN with a path prefix and a port, and that a malformed DSN's error never contains the key.
- **`pub fn signal_url(&self, signal: Signal) -> String`** gives `format!("{}/v1/{}", self.base, signal.path())`, with `Signal` being `Traces | Logs | Metrics`.
- **`pub(crate) fn http_client() -> reqwest::Client`** builds `reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(EXPORT_TIMEOUT).build()`, where `EXPORT_TIMEOUT` is 10 s. Map the error to `OkfError::Config`.
- **The exporters**, as the probe compiled them:

```rust
// `use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};` brings the
// builder methods into scope (Rust traits must be imported to call their
// methods, unlike Python's duck typing).
let spans = opentelemetry_otlp::SpanExporter::builder()
    .with_http()
    .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
    .with_endpoint(target.signal_url(Signal::Traces))
    .with_headers(target.headers.clone())
    .with_http_client(http_client()?)
    .with_timeout(EXPORT_TIMEOUT)
    .build()
    .map_err(|error| OkfError::Config { path: "[telemetry.otlp]".to_string(), message: format!("OTLP span exporter: {error}") })?;
```

`LogExporter` is the same with `Signal::Logs`. `MetricExporter` uses `Signal::Metrics` and adds `.with_temporality(Temporality::Cumulative)`; Task 2 uses it. `OkfError::Config { path: String, message }` has no `Option` path: telemetry problems use the path `[telemetry.otlp]` (`[telemetry]` from `install`).

- **Counting exporters:** `CountingSpanExporter<E>` and `CountingLogExporter<E>`, exactly the probe's `CountingExporter`/`CountingLogExporter`, with a shared `ExportFailures` instead of a bare `AtomicU64`.
  - `ExportFailures` holds one `AtomicU64` per `(Signal, FailureReason)`, where `FailureReason` is `Timeout | Transport | Rejected | Other`.
  - It is classified from `OTelSdkError`: `Timeout(_)` is `Timeout`; `InternalFailure(message)` whose message contains `"HTTP export failed"` is `Transport` when it also contains `"network"` or `"connect"`, and `Rejected` otherwise. Everything else is `Other`.
  - Task 2 renders the counter on `/metrics` as `okf.telemetry.export.failures{signal, reason}` through an observable counter over these atomics.
  - A test asserts each `OTelSdkError` variant's reason.

- [ ] **Step 6: `src/telemetry/json.rs`, the JSON line format with trace ids**

This is the probe's `DispatchSlot`, `CaptureDispatch` and `JsonWithTrace`, unchanged in substance. Additions:
- the fields `timestamp` (RFC 3339 UTC from the event time, `chrono::Utc::now()`), `level`, `target`, `message`, the event's fields, `trace_id` and `span_id`;
- `record_i64`, `record_u64`, `record_bool` and `record_f64` visitors, so numbers stay JSON numbers.

Unit test: under `tracing::subscriber::with_default`, an event inside a span on the OTel layer writes one line that parses as JSON and carries a 32-hex `trace_id` and a 16-hex `span_id`. A second test: an event outside any span has no `trace_id` key.

- [ ] **Step 7: `src/telemetry/subscriber.rs`, the layers and their caps**

```rust
/// Targets the OTLP layers never take: the export stack's own events
/// (`internal-logs` uses each crate's package name as target, some with
/// hyphens) and the HTTP stack it sends with. An export failure exported
/// over the export that failed would loop.
const EXPORT_STACK: [&str; 10] = [
    "opentelemetry", "opentelemetry_sdk", "opentelemetry-otlp", "opentelemetry-http",
    "opentelemetry-prometheus", "opentelemetry-appender-tracing",
    "hyper", "hyper_util", "reqwest", "h2",
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
/// the AWS SDK and the export stack (`EXPORT_STACK`) have their own
/// (review of PR #459, round 3). `Targets` picks the most specific matching
/// target, so `LOCAL_ONLY` (a sub-target of `okf_mcp`) stays off.
fn cap(otlp: bool) -> Targets {
    if otlp {
        return Targets::new()
            .with_default(LevelFilter::OFF)
            .with_target("okf_mcp", LevelFilter::TRACE)
            .with_target(LOCAL_ONLY, LevelFilter::OFF);
    }
    Targets::new().with_default(LevelFilter::TRACE).with_target("rmcp", RMCP_CAP)
}

/// The filters and format the layers use, already resolved (`RUST_LOG`
/// applied), so `build` is a pure function of its inputs.
#[derive(Debug, Clone)]
pub struct LayerSettings {
    pub format: LogFormat,
    pub stderr: EnvFilter,
    pub traces: EnvFilter,
    pub logs: EnvFilter,
}

/// The subscriber: stderr, plus the OTLP span and log layers when their
/// providers exist. No filter on the whole stack (spec Decision 3): each
/// layer has its own, so a quiet stderr never silences the exporters.
pub fn build(
    settings: LayerSettings,
    tracer: Option<&SdkTracerProvider>,
    logger: Option<&SdkLoggerProvider>,
) -> impl tracing::Subscriber + Send + Sync {
    // `FilterExt::and` combines two filters: an event passes when both pass.
    // `Option<Layer>` is itself a layer that does nothing when `None`.
    let slot = DispatchSlot::default();
    let (text, json) = match settings.format {
        LogFormat::Text => (Some(fmt::layer().with_writer(std::io::stderr).with_filter(settings.stderr.and(cap(false)))), None),
        LogFormat::Json => (None, Some(fmt::layer().with_writer(std::io::stderr).event_format(JsonWithTrace(slot.clone())).with_filter(settings.stderr.and(cap(false))))),
    };
    let spans = tracer.map(|provider| {
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("okf-mcp"))
            .with_error_records_to_exceptions(true)
            .with_location(false)
            .with_threads(false)
            .with_target(false)
            .with_tracked_inactivity(false)
            .with_filter(settings.traces.and(cap(true)))
    });
    let logs = logger.map(|provider| {
        OpenTelemetryTracingBridge::new(provider).with_filter(settings.logs.and(cap(true)))
    });
    Registry::default().with(CaptureDispatch(slot)).with(text).with(json).with(spans).with(logs)
}
```

Also `pub fn settings(config: &TelemetryConfig, rust_log: Option<&str>) -> Result<LayerSettings, String>`:
- **stderr** comes from `rust_log`, else `config.stderr.filter`, else `DEFAULT_STDERR_FILTER`. An invalid `RUST_LOG` falls back to the next source, as today.
- **traces and logs** come from `config.otlp`'s filters, else `DEFAULT_OTLP_FILTER`.
- **Directive components are trimmed** before parsing (tracing-subscriber 0.3.23 panics on a component with leading whitespace before a multibyte character).

Tests check the filters through `Filter::callsite_enabled`/`would_enable`, not `enabled!` (whose callsite cache is shared across subscribers):
- `stderr_at_warn_still_exports_debug_spans`: stderr `warn` and traces `debug`. A `debug` span reaches the in-memory span exporter, and nothing reaches the captured stderr writer.
- `rmcp_debug_never_reaches_any_layer`: every filter at `trace`. An `rmcp`-target `debug` event reaches neither the log exporter nor stderr.
- `export_stack_events_reach_stderr_only`: an event with target `opentelemetry-otlp` at `error` reaches stderr and not the log exporter.
- `local_only_events_reach_stderr_only`: the same for target `LOCAL_ONLY`, at every level, on both OTLP layers (a `LOCAL_ONLY` event inside a span also adds no span event).
- `dependency_events_never_reach_otlp`: with both OTLP filters at `trace`, events and spans with targets `sqlx::query`, `apalis`, `openidconnect`, `aws_config`, `tower_http` and `rmcp` reach neither OTLP exporter, while an `okf_mcp::store` event does. Check first that `Targets`' most-specific-match rule holds for `okf_mcp::local_only` against `okf_mcp`, and if it does not, AND a second `Targets` that turns `LOCAL_ONLY` off.
- **Note:** `tower_http`'s per-response `info` line is not exported. The request span carries the same status and latency. `EXPORT_STACK` stays only as documentation of why the export stack must never be allow-listed.
- `error_event_marks_span_error`: an `error!` inside a span exports the span with `Status::Error`.

Each test builds the subscriber with in-memory exporters and `with_simple_exporter`, and runs under `tracing::subscriber::with_default`. Keep each provider alive until the end of its test, then `shutdown()`. Dropping a provider inside the dispatcher's lock has deadlocked other projects' tests.

- [ ] **Step 8: `src/telemetry/mod.rs`, `Telemetry::start` and `shutdown`**

```rust
/// The running telemetry of `serve`: the providers, kept for shutdown.
#[derive(Debug)]
pub struct Telemetry {
    tracer: Option<SdkTracerProvider>,
    logger: Option<SdkLoggerProvider>,
    failures: Arc<ExportFailures>,
    // Task 2 adds `meter: SdkMeterProvider` and `registry: prometheus::Registry`.
}

/// How long shutdown waits for the last flushes (inside Kubernetes' default
/// 30 s grace, after the HTTP drain).
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(4);

impl Telemetry {
    /// Builds the resource, the exporters and the providers for `serve`, sets
    /// the global tracer provider and the W3C propagator. An exporter that
    /// cannot be built is logged (to stderr) and its signal stays off.
    ///
    /// # Arguments
    /// * `config` - `[telemetry]`, already validated.
    /// * `env` - reads an environment variable (injected for tests).
    ///
    /// # Returns
    /// The running telemetry.
    ///
    /// # Errors
    /// [`OkfError::Config`] when a variable named by `headers_env` or
    /// `sentry_dsn_env` is unset or blank, or its value does not parse.
    pub fn start(config: &TelemetryConfig, env: &dyn Fn(&str) -> Option<String>) -> Result<Self, OkfError> { .. }

    /// The subscriber for `serve`: stderr plus whichever OTLP layers started.
    pub fn subscriber(&self, settings: LayerSettings) -> impl tracing::Subscriber + Send + Sync { .. }

    /// Flushes and shuts down every provider in parallel, each on a plain
    /// thread, within [`SHUTDOWN_DEADLINE`]. A flush still running at the
    /// deadline is abandoned and named on stderr. Idempotent.
    ///
    /// # Returns
    /// Each provider's outcome (`Ok`, failed, or abandoned at the deadline),
    /// which Task 7's sentinel test asserts on.
    pub async fn shutdown(self) -> ShutdownReport { .. }
}
```

Implementation notes:
- **Resource:** `Resource::builder().with_service_name(config.service_name.clone()).with_attributes(..)`, with `service.version` = `env!("CARGO_PKG_VERSION")` and `deployment.environment.name` when set. Env detectors stay on, so `OTEL_RESOURCE_ATTRIBUTES` adds `k8s.*`.
- **`OTEL_SDK_DISABLED=true`** (read through `env`) means OTLP off, logged once.
- **Sampler:** `ParentBased(TraceIdRatioBased(r))` when `sample_ratio` is set. Otherwise leave the SDK default, which reads `OTEL_TRACES_SAMPLER`.
- **Globals:** `global::set_tracer_provider(tracer.clone())` and `global::set_text_map_propagator(TraceContextPropagator::new())`.
- **`shutdown`:** take each provider. In `tokio::task::spawn_blocking`, spawn one `std::thread` per provider (tracer, then logger) running `provider.shutdown()`. Wait on a `std::sync::mpsc` channel with `recv_timeout` until the deadline, then log `tracing::warn!(target: "okf_mcp::telemetry", provider = name, "telemetry flush abandoned at shutdown")`; it reaches stderr, whose layer is still installed.
  - Ignore `Err(OTelSdkError::AlreadyShutdown)`.
  - Plain threads because the tokio batch processor ignores the timeout given to `shutdown_with_timeout`, and a dead receiver costs several retry timeouts.

Tests:
- `shutdown_returns_within_the_deadline_with_a_hung_exporter`: a `SpanExporter` whose `export` never completes, wrapped by `BatchSpanProcessor`. `shutdown` returns in under `SHUTDOWN_DEADLINE + 1s` (`tokio::time::timeout`).
- `second_shutdown_is_quiet`.
- `start_without_otlp_builds_no_providers`.
- `start_with_unset_headers_env_names_the_variable`: the error contains the variable's name and no value.

- [ ] **Step 9: Wire `serve` and the other commands**

`src/main.rs` drops its subscriber (`DEFAULT_LOG_FILTER` moves to `telemetry::config::DEFAULT_STDERR_FILTER`). `cli::run` installs one:
- **Before `Config::load`**, set a temporary stderr subscriber, **scoped to start-up only**: `fallback()` is stderr text at `DEFAULT_STDERR_FILTER` (or `RUST_LOG`). Install it with `tracing::subscriber::set_default` inside a block that covers `Config::load` and, for `serve`, `Telemetry::start`, and **drop its guard before installing the global subscriber**. `set_global_default` does not override a thread-local default that is still active, so a kept guard would leave the startup thread (and every future polled on it) on the fallback, without OTLP (review of PR #459). For example: `let (config, telemetry) = { let _startup = tracing::subscriber::set_default(subscriber::fallback()); ...; (config, telemetry) };`. The block's end drops `_startup`.
- **For every non-serve command**, after `Config::load`: `tracing::subscriber::set_global_default(subscriber::build(settings(&config.telemetry, rust_log)?, None, None))`.
- **For `Command::Serve`**, after `Config::load`: `let telemetry = Telemetry::start(&config.telemetry, &|name| std::env::var(name).ok())?;` then set the global default to `telemetry.subscriber(settings)`. Then the existing steps. After `http::serve(..).await` returns, `telemetry.shutdown().await`, then return `served`. The fatal error, if any, is logged inside the subscriber first, so it is exported too.

If `set_global_default` fails (already set), return `OkfError::Config` ("a tracing subscriber is already installed"). Tests that call `cli::run` must not hit this; check `grep -rn "cli::run(" tests src`.

Put the install sequence in one function, `telemetry::install(config, telemetry) -> Result<(), OkfError>`, called by `cli::run`. Regression test (its own integration-test binary, `tests/telemetry_install.rs`, since it sets the process's global subscriber): run the start-up sequence with a capturing writer, then emit an event **on the same thread** after `install`, and assert that the configured subscriber got it (the JSON format, so the line is distinguishable from the fallback's text) and the fallback did not.

> **Corrected (issue #461, 2026-11-15).** What shipped differs from the sketches above in these ways; read `src/telemetry/` for the code.
> - **`Config` is no longer `Eq`** (`sample_ratio` is an `f64`); it is still `PartialEq`.
> - **The probe crate was not in the repository**, so `DispatchSlot`, `CaptureDispatch`, `JsonWithTrace` and the counting exporters were written from the verified facts. The batch processors come from the `*_with_async_runtime` modules (`BatchSpanProcessor`, `BatchLogProcessor`), not the crate-root ones.
> - **Span-layer error options:** the layer turns off `with_error_fields_to_exceptions`, `with_error_records_to_exceptions`, `with_error_events_to_exceptions` and `with_error_events_to_status` (the plan's `with_error_records_to_exceptions(true)` put error text in `exception.message`, which the privacy rules forbid). An ERROR event still marks the span `Error`, with an empty description.
> - **Filters are built per arm in `build_with_writer`:** a filter's type names its subscriber, so `settings.stderr.and(cap(false))` cannot be shared by the text and JSON arms. `build_with_writer` and `fallback(rust_log, writer)` take the stderr writer, so tests capture it.
> - **`ShutdownReport { traces, logs }`** holds a `ProviderOutcome` each: `NotRunning`, `Flushed` (including an already shut-down provider), `Failed(message)` or `Abandoned`. `ShutdownReport::all_flushed()` is the assertion Task 7 wants. Task 2 adds a `metrics` field.
> - **Start-up sequence:** `telemetry::start_up(path, serve, env, writer) -> StartUp { config, telemetry }` runs `Config::load` and `Telemetry::start` under the fallback and drops its guard on return; `telemetry::install(&config.telemetry, telemetry.as_ref(), rust_log)` then sets the global subscriber (`install_with_writer` for tests). `cli::run` calls both; the serve body moved to `cli::serve`, so `telemetry.shutdown()` runs after a failure too, after a closed-message `tracing::error!("serve failed")` (the error's text is not exported; `main` prints it).
> - **The wire is tested** in `telemetry::tests::spans_logs_and_metrics_reach_the_receiver_with_their_headers` (renamed in issue #463; it also expects the metrics export) with a loopback receiver.

> **Corrected (issue #461, review of PR #480, 2026-11-15).**
> - **`LogMailer` moved to `LOCAL_ONLY` in Task 1** (`src/mail/log.rs`, test `the_message_reaches_stderr_and_neither_otlp_layer`): the message holds the reset link, and the allow-list would otherwise export it. Task 1b keeps the rest of the audit.
> - **The OTLP cap is `subscriber::otlp_cap`, a `filter_fn` over `is_exportable_target`** (exactly `okf_mcp`, or `okf_mcp::...`, never `okf_mcp_other`, never `LOCAL_ONLY` or under it), not `Targets`, whose directives match by string prefix. `cap(false)` is `stderr_cap()`.
> - **`OTEL_SDK_DISABLED` is checked before the OTLP target is resolved**, so a disabled SDK needs no credentials.
> - **Log correlation needs no feature:** `opentelemetry-appender-tracing` 0.33 has no `experimental_use_tracing_span_context`; the span layer's context activation (stated explicitly with `with_context_activation(true)`) gives the bridge the span's ids, tested by `an_exported_log_carries_the_enclosing_spans_ids`.
> - Test helper `subscriber::capture::observe` runs a body under in-memory exporters; Tasks 1b and 7 can use it.

- [ ] **Step 10: Gates and commit**

`tools/agentic/gate.sh fmt`, `lint`, `test`. Then commit `feat(telemetry): config, subscriber, OTLP traces and logs (M15 Task 1)`.

---

### Task 1b: The privacy audit of existing events

The log bridge exports every `tracing` event that passes its filter. `skip_all` protects only the arguments of functions instrumented from now on; it does nothing for the events the crate already emits, and some of those carry exactly what the spec's "never recorded anywhere" list forbids. Found in review of the plan PR (#459):
- `src/mail/log.rs:15-19` logs the recipient and the whole message, setup and reset links included (that is the `log` provider's purpose); **done in Task 1 (issue #461, review of PR #480): it now logs with `target: LOCAL_ONLY`, with its own test, so Task 1 never ships a reset token to OTLP. Task 1b keeps the rest, and its sentinel case for the `log` provider is now a regression check;**
- `src/oauth/authorize.rs:526` logs a caller-supplied `client_id` and the refusal's text;
- `src/http/mod.rs:136` and `src/mcp/mod.rs:137` (`tool_error`) log an internal error's full `Display`.

**No release may ship Task 1 without this task.** OTLP is off unless configured, so `main` is safe between them, but the release that turns OTLP on must contain both.

**Files:** every file with a `tracing` event that this audit changes (expect `src/mail/log.rs`, `src/oauth/*.rs`, `src/http/mod.rs`, `src/mcp/mod.rs`, `src/app.rs`, `src/http/signin.rs`, `src/oidc.rs`, `src/mail/queue.rs`); `docs/contributing/policy-lints.md` if Task 8 has merged (the event rules), otherwise the spec's "Privacy"; `tests/telemetry_privacy.rs` if Task 7 has merged (otherwise Task 7 adds the cases below).

- [ ] **Step 1: List every event.** `grep -rn "tracing::\(trace\|debug\|info\|warn\|error\)!\|\b\(trace\|debug\|info\|warn\|error\)!(" src --include=*.rs`, excluding `#[cfg(test)]` code (about 110 sites on `bc32c72`). Put the table in the PR description: site, level, each field, and its class.

- [ ] **Step 2: Classify each field** as one of:
  - **closed**: an enum's `as_str`, a `kind()`, a count, a duration, a generation, a status code, a route template, a catalog name that exists. These stay.
  - **id**: a user id, key id, session-binding id prefix, job id. These stay (the same rule as span attributes).
  - **caller text**: anything a request supplied (a client id, a redirect URI, an email, a name, a path, a query value, a header). It moves to a `LOCAL_ONLY` event or is dropped.
  - **error text**: `%error` of an `OkfError`, `sqlx::Error`, `reqwest::Error` or a provider error. Its message can name a path or echo input. The exported event keeps `kind`, a closed value; the full text moves to a `LOCAL_ONLY` event beside it. On stderr that is two lines, one per sink, and the detail line says what the generic one does.
  - **content**: a document, a mail body, a link. These are `LOCAL_ONLY` only.

- [ ] **Step 3: Apply.** For example:

```rust
// Before: tracing::error!(error = %self.0, "request failed");
// After: the exported event says what failed by kind; the detail, which can
// name a path or echo a value, stays on stderr (`LOCAL_ONLY` is turned off
// on both OTLP layers).
tracing::error!(kind = self.0.kind(), "request failed");
tracing::error!(target: LOCAL_ONLY, error = %self.0, "request failed (detail)");
```

`LogMailer::send` already logs with `target: LOCAL_ONLY` alone (Task 1). `authorize.rs:526` becomes `warn!(reason = error.kind(), "refused an OAuth client")` plus a `LOCAL_ONLY` line with the `client_id` and the text. If the error type has no `kind`, give it one (a closed `&'static str`).

> **Corrected (issue #462, 2026-11-15).** The audit shipped as written, with these differences.
> (1) Sites use the macro `split_event!` (`src/telemetry/subscriber.rs`) instead of two hand-written statements: `split_event!(warn, { exported fields, }, { local fields, }, "message")` emits the exported event and a `LOCAL_ONLY` twin ending in `(detail)`. A message with a `{captured}` argument cannot go through it (`concat!` loses the capture), so `log_pruned_retrying` and `note_failure` in `src/app.rs` write both events out. (2) Principal text counts as caller text: `actor` (a user name), `user = %user.name` and `key`/`key_id` (a console session's key id is `session:<name>`) moved to the `LOCAL_ONLY` twin of every audit-style event; the exported event keeps ids, counts and closed values. (3) `authorize.rs` exports `reason = error.code()` (a closed `&'static str`; `OAuthError` has no `kind`). (4) Events whose target is outside `okf_mcp` (the investigator's `okf_maintenance::rejection`, the `okf-maintenance` binary) never reach the OTLP allow-list and were left alone. (5) The per-site unit tests sit beside the sites (`an_internal_request_error_...` in `src/http/mod.rs`, `an_internal_tool_error_...` in `src/mcp/mod.rs`, `a_refused_client_id_...` in `src/oauth/authorize.rs`, `a_split_event_...` in `src/telemetry/subscriber.rs`). Task 7 still adds the end-to-end sentinel cases below. The classification table is in the PR description of issue #462.

- [ ] **Step 4: Test.**
  - Unit tests per changed site, using the in-memory log exporter: a refused OAuth client whose id contains a sentinel exports no record containing it; the `log` mailer's send exports nothing; an internal error exports `kind` and not its text.
  - The sentinel test (Task 7) gains these cases: the `log` mail provider with a sign-up invitation, a refused OAuth client id, and an internal error whose text holds the sentinel. Add them here if Task 7 has merged, or note them on Task 7's issue.
  - **Revert check:** put one site back and see its test fail.

- [ ] **Step 5: The rule for new code.** The policy doc gains "Events": the same classes; caller text, error text and content go to `LOCAL_ONLY`. Review checks every new event against it.

- [ ] **Step 6: Gates and commit** `fix(telemetry): keep caller text, error text and mail content out of OTLP (M15 Task 1b)`

---

### Task 2: The metrics provider and the `/metrics` listener

**Files:**
- Create: `src/telemetry/metrics.rs`, `src/telemetry/scrape.rs`
- Modify: `Cargo.toml`, `src/telemetry/mod.rs`, `src/telemetry/config.rs`, `src/config.rs` (overlap check), `src/app.rs` (`metrics` field, `load_with_metrics`), `src/cli.rs` (spawn the listener)

- [ ] **Step 1: Dependencies**

```toml
# Prometheus `/metrics` from the same SdkMeterProvider that pushes OTLP
# (spec Decision 1): this reader renders into a `prometheus::Registry`. It
# turns on opentelemetry_sdk's `experimental_metrics_custom_reader`, which is
# what lets it mount as a reader, and it is why every opentelemetry* crate is
# held at its release (0.33): a reader from another SDK major does not compile.
opentelemetry-prometheus = "0.33.1"
# The registry the reader renders into, and its text encoder. No protobuf.
prometheus = { version = "0.14.0", default-features = false }
```

- [ ] **Step 2: Config**

`TelemetryConfig.prometheus: Option<PrometheusConfig>`, where `PrometheusConfig { listen: SocketAddr }` has `deny_unknown_fields`.

`Config::validate` refuses `prometheus.listen == self.listen`, and also equal ports when either IP is unspecified (`0.0.0.0` / `::`). The message is "telemetry.prometheus.listen overlaps listen". Test: the three overlap cases, plus a distinct port that passes.

- [ ] **Step 3: Write the failing scrape test** (`src/telemetry/scrape.rs`)

```rust
#[tokio::test]
async fn every_family_is_present_before_any_request() {
    let (meter, registry) = test_meter();          // a provider with only the Prometheus reader
    let metrics = Metrics::new(&meter);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let shutdown = CancellationToken::new();
    let served = tokio::spawn(serve(listener, registry, shutdown.clone()));
    let body = reqwest::get(format!("http://{address}/metrics")).await.expect("get").text().await.expect("body");
    for family in Metrics::FAMILIES {             // the Prometheus names, a const list in metrics.rs
        assert!(body.contains(&format!("# TYPE {family} ")), "{family} missing:\n{body}");
    }
    assert!(!body.contains("target_info") && !body.contains("otel_scope_"));
    // A family declared twice makes Prometheus reject the whole scrape.
    let types: Vec<&str> = body.lines().filter(|l| l.starts_with("# TYPE ")).collect();
    let unique: std::collections::BTreeSet<&&str> = types.iter().collect();
    assert_eq!(types.len(), unique.len());
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), served).await.expect("stops").expect("join").expect("serves");
}
```

Also:
- `other_paths_are_404`;
- `seconds_histograms_use_the_seconds_buckets`: after one `tool_call(.., 0.042)`, the `le="0.05"` bucket is 1;
- `the_scrape_never_writes_a_partial_body`: encode into a buffer first, and answer 500 on an error.

A family appears only once it has a point. So `Metrics::FAMILIES` lists only the families that exist before any traffic: the observable ones (`okf_build_info`, `okf_telemetry_export_failures_total`, and, as Tasks 4-6 add them, the catalog, changeset, mail-queue, pool and clock gauges). Families recorded on events (request, tool, publish, auth and mail counters and histograms) are checked by each task's own test after one event. Nothing is seeded with fake observations.

- [ ] **Step 4: `src/telemetry/metrics.rs`**

```rust
/// Seconds-histogram boundaries: 1 ms to 60 s. The SDK's defaults are
/// scaled for milliseconds (Verified facts).
pub const SECONDS_BUCKETS: [f64; 14] =
    [0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 60.0];

/// What an operation ended as: a closed set, so a label never grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome { Ok, Error }

impl Outcome {
    /// The label value.
    pub const fn as_str(self) -> &'static str { match self { Self::Ok => "ok", Self::Error => "error" } }
}

/// The server's instruments. Callers use the typed methods only, never an
/// instrument or a raw attribute (spec "The `Metrics` handle").
#[derive(Debug, Clone)]
pub struct Metrics {
    // `Arc` so `App` and the scrape callbacks share one set cheaply.
    inner: Arc<Instruments>,
}

#[derive(Debug)]
struct Instruments {
    build_info: Gauge<u64>,
    // Tasks 3-6 add theirs here, one field per instrument.
}

impl Metrics {
    /// The Prometheus names `Metrics::new` makes present at once (tests).
    pub const FAMILIES: &'static [&'static str] = &["okf_build_info", "okf_telemetry_export_failures_total"];

    /// Builds every instrument on `meter`.
    pub fn new(meter: &Meter) -> Self { .. }

    /// The instruments on the global meter provider (no-op until `serve` sets one).
    pub fn global() -> Self { Self::new(&opentelemetry::global::meter("okf-mcp")) }

    /// Registers `okf.telemetry.export.failures` over `failures`.
    pub fn observe_export_failures(&self, meter: &Meter, failures: Arc<ExportFailures>) { .. }
}
```

`okf.build.info` is a `u64` gauge set to 1 with attribute `version = env!("CARGO_PKG_VERSION")`.

A tool-name label is only ever one of a fixed list. `pub fn tool_label(name: &str) -> &'static str` returns the matching `&'static str` from `crate::mcp::annotations::{READS, STAGED, DESTRUCTIVE}`, or `"other"`. Task 3 uses it. If those constants are not `&[&str]`, adapt and record it.

- [ ] **Step 5: The provider in `Telemetry::start`**

`Telemetry` gains `meter: SdkMeterProvider` and `registry: prometheus::Registry`. Every `serve` builds:
- the Prometheus reader: `opentelemetry_prometheus::exporter().with_registry(registry.clone()).without_target_info().scope_info_enabled(false).build()`;
- when OTLP is on, the periodic reader: `periodic_reader_with_async_runtime::PeriodicReader::builder(CountingMetricExporter { .. }, runtime::Tokio).with_interval(otlp.export_interval).build()`.

Then `SdkMeterProvider::builder().with_resource(resource).with_reader(..)`, and `global::set_meter_provider(meter.clone())` **before** `App::load`. `CountingMetricExporter<E: PushMetricExporter>` counts failures like the others. `shutdown` adds the meter provider to its parallel set.

- [ ] **Step 6: `App::load_with_metrics`**

`App` gains `metrics: Metrics` and `pub fn metrics(&self) -> &Metrics`.
- `App::load(pool, config, clock)` becomes `Self::load_with_metrics(pool, config, clock, Metrics::global()).await`.
- `load_with_metrics` is the old body plus the field.

Test helper: `mcp::testing::app_with_metrics(catalogs_and_files, config) -> (TestApp, prometheus::Registry)` builds a local provider with only the Prometheus reader, which Tasks 3-6 use to assert metrics from a real `App`. `testing::render(&registry) -> String`.

- [ ] **Step 7: `src/telemetry/scrape.rs`, the listener**

```rust
/// Serves `GET /metrics` on its own listener until `shutdown` (spec
/// Decision 2): no authentication, never routed by the ingress.
///
/// # Errors
/// The listener's I/O error.
pub async fn serve(listener: TcpListener, registry: prometheus::Registry, shutdown: CancellationToken) -> std::io::Result<()> {
    let router = Router::new().route("/metrics", get(render)).with_state(registry);
    axum::serve(listener, router).with_graceful_shutdown(async move { shutdown.cancelled().await }).await
}
```

`render` encodes `registry.gather()` with `TextEncoder` into a `Vec<u8>`.
- On success it answers 200 with `content-type: text/plain; version=0.0.4`.
- On an error it logs and answers 500 with no body.

In `cli.rs`'s serve arm, when `config.telemetry.prometheus` is set:
1. bind its listener before `http::serve`, failing start-up on a bind error;
2. `tokio::spawn(telemetry::scrape::serve(listener, telemetry.registry(), shutdown.clone()))`;
3. await it after `http::serve` returns, before `telemetry.shutdown()`.

- [ ] **Step 8: Gates and commit**

`feat(telemetry): metrics provider and /metrics listener (M15 Task 2)`

> **Corrected (issue #463, 2026-11-15).** What shipped differs from the sketches above in these ways; read `src/telemetry/{metrics,scrape,mod,otlp,config}.rs` for the code.
> - **`observe_export_failures` is a free function** `telemetry::metrics::observe_export_failures(meter, failures)`, not a `Metrics` method: it needs no handle. `Telemetry::start` calls it, so `okf_telemetry_export_failures_total` is on `/metrics` for every `serve` (all 12 `signal` x `reason` series, zero until a failure). It is an observable counter, and its returned handle is dropped: the callback stays registered with the provider (tested by `a_counted_failure_shows_on_the_next_scrape`), so Task 4's note about keeping `ObservableGauge` handles on `App` is optional, not needed for the callbacks to live.
> - **`Metrics::new(meter)` records only `okf.build.info`** (a `u64` gauge, `version` label). So `Metrics::FAMILIES` (`okf_build_info`, `okf_telemetry_export_failures_total`) is present only after both `Metrics::new` and `observe_export_failures`; `Telemetry::start` does the second, `App::load` the first (through `Metrics::global()`). Tests build both on a local provider (`scrape::tests::test_meter`).
> - **`Telemetry` holds `meter: SdkMeterProvider` (always) and `registry`**, and `Telemetry::registry()` returns a clone. The Prometheus reader is built for every `serve`, with or without OTLP, and with `OTEL_SDK_DISABLED=true` (that switch turns the OTLP exporters off only). The OTLP periodic reader is added when OTLP is on; `otlp::metric_exporter` builds the cumulative OTLP metric exporter and `CountingMetricExporter` counts its failures under `Signal::Metrics`.
> - **`ShutdownReport { traces, logs, metrics }`**: the meter provider shuts down in the same parallel set (its OTLP reader makes a final export, tested by `metrics_reach_the_receiver_when_the_provider_shuts_down`). `all_flushed()` covers all three.
> - **`mcp::annotations` is no longer test-only**: `READS`, `STAGED` and `DESTRUCTIVE` are `pub(crate)` so `metrics::tool_label` can read them (they are `&[&str]`, as assumed). Only the test helpers in that file stay `#[cfg(test)]`.
> - **`seconds_histogram(meter, name, description)`** in `metrics.rs` builds a seconds histogram with `SECONDS_BUCKETS`; Task 3's instruments use it. `Outcome` and `SECONDS_BUCKETS` are as sketched. `Instruments` is empty until Task 3 adds fields; `Metrics.inner` carries an `allow(dead_code)` that Task 3 removes.
> - **`scrape::serve`** renders through a `Gather` closure over `registry.gather()` (so the 500 path is testable); the signature in Step 7 is unchanged. In `cli.rs` the listener is bound after the main one and before `http::serve`, spawned with `Telemetry::registry()` (passed into `cli::serve` as `registry: Option<prometheus::Registry>`), and, after `http::serve` returns on any path, the shutdown token is cancelled and the scrape task awaited before `dispatch` calls `telemetry.shutdown()`.
> - **The overlap check also runs for `serve --listen`** (`Config::validate_metrics_listen`), since the file's check saw the file's address. The message is the same. `PrometheusConfig::overlaps` holds the rule; `[::]:8080` against `127.0.0.1:8080` counts as overlapping.
> - **`mcp::testing::app_with_metrics`** returns `(TestApp, prometheus::Registry)`; `TestApp` keeps the local provider alive (`_meter`). `testing::render(&registry)` is as sketched.

---

### Task 3: HTTP and MCP

**Files:**
- Create: `src/telemetry/propagation.rs`
- Modify: `src/http/mod.rs` (`request_span`, the response hook), `src/auth.rs` and `src/http/console_session.rs` (record principal ids), `src/mcp/mod.rs` (`call_tool`), `src/telemetry/metrics.rs`

- [ ] **Step 1: Write the failing tests**

In `src/http/mod.rs`, using a test subscriber with an in-memory span exporter:
- **`a_request_span_names_method_and_route`.** `GET /api/v1/catalogs/{catalog}/health` gives one span with:
  - name `GET /api/v1/catalogs/{catalog}/health`;
  - `http.request.method = "GET"` and `http.route` = the template;
  - `http.response.status_code` = `I64(200)`;
  - status not Error;
  - no attribute value containing the catalog name typed in the path, the query, or any header value.
- **`a_5xx_is_an_error_and_a_4xx_is_not`.** A 500 (database unreachable `/healthz`, as the existing test arranges) is Error; a 404 from a known route is not.
- **`an_unmatched_path_makes_no_span`.** `GET /no/such/path` exports nothing.
- **`a_forged_traceparent_is_a_link_not_the_parent`.** With `traceparent: 00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01`:
  - the span's trace id is **not** `0af7..`;
  - its parent is none;
  - it has one link to that trace.
- **Every test above runs at two span filters, `info` and `trace`**, through one helper that takes the filter, so Task 10 can re-run them over the swept tree. At `trace` it must still hold that the request span is a root and an unmatched path exports nothing.
- **`an_authenticated_request_records_principal_ids`.** A keyed request records `enduser.id` (the key's user id) and `okf.key.id`, and never the bearer value.

In `src/mcp/mod.rs`:
- **`every_tool_call_opens_one_mcp_span`.** `search` through a real MCP session gives a span named `mcp search` with `okf.tool = "search"`, `okf.outcome = "ok"`, and `okf.catalog` when the arguments name a loaded catalog.
- **`a_tool_error_records_its_kind_not_its_message`.** An unknown path gives `okf.outcome = "error"`, `okf.error.kind = "NotFound"` (or whatever `kind()` says), and no attribute containing the path.
- **`an_unknown_tool_name_is_labelled_other`** (metrics).
- **`tool_duration_is_recorded_per_tool_catalog_outcome`**, through `app_with_metrics`.

- [ ] **Step 2: `request_span` and the response hook**

```rust
fn request_span<B>(request: &axum::http::Request<B>) -> tracing::Span {
    // No route matched: no span, so scanners probing random paths do not
    // create a trace each (they still get the 404).
    let Some(route) = request.extensions().get::<MatchedPath>().map(MatchedPath::as_str) else {
        return tracing::Span::none();
    };
    let method = semconv_method(request.method());   // GET..PATCH as is; anything else "_OTHER"
    // `parent: None`: the request is always a root, never the child of
    // whatever span is current (review of PR #459, round 3). This function
    // is exempt from `uninstrumented_fn` for the same reason: an
    // `#[instrument]` here would make its own span the parent of every
    // request, and export a span even for an unmatched path.
    let span = tracing::info_span!(
        parent: None,
        "request",
        otel.name = %format_args!("{method} {route}"),
        otel.kind = "server",
        otel.status_code = tracing::field::Empty,
        http.request.method = method,
        http.route = route,
        http.response.status_code = tracing::field::Empty,
        enduser.id = tracing::field::Empty,
        okf.key.id = tracing::field::Empty,
    );
    if let Some(link) = crate::telemetry::propagation::link_from(request.headers()) {
        // `OpenTelemetrySpanExt::add_link`: the caller's trace is linked,
        // never adopted (spec Decision 4).
        span.add_link(link);
    }
    span
}
```

Notes:
- The existing `tower_http` response line logs `path` today. Keep a `path = route` field if the log-capture test (`src/http/mod.rs:424-501`) asserts on it, and update that test otherwise.
- Replace `DefaultOnResponse` with an `on_response` closure that records:
  - `http.response.status_code = i64::from(response.status().as_u16())`;
  - `otel.status_code = "error"` for 5xx;
  - the existing `info` line, as `DefaultOnResponse` did.
- `Metrics::http_request(method, route, status, elapsed)` records `http.server.request.duration` (`SECONDS_BUCKETS`). An `on_request`/drop guard adjusts `http.server.active_requests` (an `i64_up_down_counter`, attributes `http.request.method` and `http.route`). The unmatched case records nothing.

`propagation::link_from(headers) -> Option<SpanContext>` is the probe's `extract_link`.

- [ ] **Step 3: Principal ids**

Root-span fields are recorded **on a handle to the root span, never on `Span::current()`**. Once the sweep (Task 9) instruments the auth middleware and its helpers, and an operator raises `traces_filter`, `Span::current()` is a child span that does not declare `enduser.id`, and `record` silently does nothing (review of PR #459).

A one-line middleware, layered directly inside `TraceLayer` (in axum the last `.layer(..)` is the outermost, so it goes just before the `TraceLayer` call), stores the request span in the request's extensions:

```rust
/// The request span `TraceLayer` opened, for code deeper in the stack that
/// records the request's root fields (the principal's ids).
#[derive(Debug, Clone)]
pub struct RequestSpan(pub tracing::Span);

// Exempt from `uninstrumented_fn`: its own span would become `Span::current()`
// and it would capture that instead of the request span.
async fn capture_request_span(mut request: Request, next: Next) -> Response {
    request.extensions_mut().insert(RequestSpan(tracing::Span::current()));
    next.run(request).await
}
```

At `auth.rs:1192` and both inserts in `console_session.rs`:

```rust
if let Some(RequestSpan(span)) = request.extensions().get::<RequestSpan>() {
    span.record("okf.key.id", principal.key_id.as_str());
    if let Some(user_id) = principal.user_id.as_deref() {
        span.record("enduser.id", user_id);
    }
}
```

Test `principal_ids_land_on_the_request_span_under_helper_spans`: with the span filter at `trace`, and the middleware wrapped in an extra `trace_span!` in the test (standing in for the sweep's `#[instrument]`), the exported request span still carries `enduser.id` and `okf.key.id`.

- [ ] **Step 4: `OkfServer::call_tool`**

Inside `#[tool_handler(router = self.tool_router)] impl ServerHandler for OkfServer`:

```rust
async fn call_tool(
    &self,
    request: rmcp::model::CallToolRequestParams,
    context: rmcp::service::RequestContext<rmcp::RoleServer>,
) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
    let tool = crate::telemetry::metrics::tool_label(&request.name);
    let catalog = self.catalog_label(request.arguments.as_ref());   // a loaded catalog's name, else "none"
    let span = tracing::info_span!(
        "mcp",
        otel.name = %format_args!("mcp {tool}"),
        otel.kind = "server",
        otel.status_code = tracing::field::Empty,
        okf.tool = tool,
        okf.catalog = catalog.as_str(),
        okf.outcome = tracing::field::Empty,
        okf.error.kind = tracing::field::Empty,
        enduser.id = tracing::field::Empty,
        okf.key.id = tracing::field::Empty,
    );
    // Principal ids come from the request's parts, as `ToolContext::from_parts` reads them.
    record_principal(&span, &context);
    let started = std::time::Instant::now();
    // `Instrument::instrument` enters the span on every poll of the future,
    // so the tool's own spans and events are its children.
    let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
    let result = self.tool_router.call(tcc).instrument(span.clone()).await;
    let outcome = outcome_of(&result);       // Ok(Complete(r)) with is_error != Some(true) -> Ok
    span.record("okf.outcome", outcome.as_str());
    if outcome == Outcome::Error { span.record("otel.status_code", "error"); }
    self.app.metrics().tool_call(tool, &catalog, outcome, started.elapsed());
    result
}
```

- **The error kind** is recorded on the tool span through a task-local handle, not `Span::current()`. Under a raised filter, `Span::current()` in `tool_error` is the tool method's own `debug` span or a helper's (review of PR #459).

  ```rust
  // `task_local!` is a value scoped to one async task, like a
  // `contextvars.ContextVar` in Python.
  tokio::task_local! {
      /// The span of the tool call in progress (set by `call_tool`).
      pub(crate) static TOOL_SPAN: tracing::Span;
  }
  ```

  `call_tool` runs `TOOL_SPAN.scope(span.clone(), self.tool_router.call(tcc).instrument(span.clone())).await`. `tool_error` does `let _ = TOOL_SPAN.try_with(|span| span.record("okf.error.kind", error.kind()));`, which is a no-op outside a tool call. If the router turns out to run the tool on another task (the scope would then be lost), record the kind in `call_tool` instead, from `structured_content["error"]` matched against `OkfError::kind()`'s strings, and say so in this plan.
  - Test `error_kind_lands_on_the_tool_span_under_helper_spans`: with the span filter at `trace` and the tool method wrapped in a `debug` span (as the sweep will), a failing tool's exported `mcp <tool>` span carries `okf.error.kind`.
  - `record_principal(&span, &context)` writes to the `span` handle `call_tool` holds, for the same reason.
- **`catalog_label`** reads `arguments["catalog"]` as a string and returns it only if `self.app.catalog(name)` is loaded, otherwise `"none"`. An unloaded name never becomes a label.
- **`okf.mcp.tool.duration`** is an `f64_histogram`, `SECONDS_BUCKETS`, attributes `tool`, `catalog`, `outcome`.
- **`okf.mcp.sessions.active`** is an `i64_up_down_counter`, incremented in the `mcp_service` factory (`src/http/mod.rs:227`) and decremented when the `OkfServer` is dropped (`impl Drop for OkfServer`). Test: open and close a session, and the gauge returns to 0.

- [ ] **Step 5: Gates and commit**

`feat(telemetry): HTTP request spans, MCP tool spans and their metrics (M15 Task 3)`

> **Corrected (issue #464, 2026-11-16).** What shipped differs from the sketches above in these ways; read `src/http/mod.rs`, `src/mcp/mod.rs`, `src/auth.rs` and `src/telemetry/{propagation,metrics}.rs` for the code.
> - **`okf.key.id` is `session` for a console session** (decision, 2026-11-16). A session's `Principal.key_id` is `session:<user name>`, and a name never goes in a span attribute. `auth::span_key_id` maps it (the kind only); API keys and OAuth grants record their real key id; `Principal.key_id` itself is unchanged. `auth::record_principal_ids(span, principal)` is the one helper both spans use, and `auth::record_on_request_span(extensions, principal)` is what `require_key` and `require_principal` call (the plan's inline `span.record` blocks are replaced by these). `enduser.id` is `Principal.user_id`.
> - **One middleware, not two hooks.** `request_telemetry` (in `src/http/mod.rs`, inside `TraceLayer`) stores `RequestSpan` and also measures the request: `Metrics::http_in_flight(method, route)` returns a drop guard (no `on_request` hook), and `Metrics::http_request(method, route, status, elapsed)` records `http.server.request.duration` with `http.response.status_code` as a label. `record_response` replaces `DefaultOnResponse` and calls it, so the `info` line is unchanged. An unmatched route is neither spanned nor measured.
> - **The request span's log fields changed.** `method`/`path`/`version` became `http.request.method`/`http.route` (and the others above); an unmatched request has no span, so the `<unmatched>` placeholder and `UNMATCHED_ROUTE` are gone. The log-capture test now asserts `http.route=` and the absence of `<unmatched>`.
> - **`propagation::link_from` keeps ids and flags only**; the header's `tracestate` is dropped, so no header text is exported on a link. Task 6 adds `inject` beside it and needs no change.
> - **`call_tool` types:** `CallToolRequestParams` and `CallToolResponse` (rmcp 3.8). `#[tool_handler]` skips a `call_tool` already written. The session gauge is held by a `SessionGuard` field in `OkfServer`, created in `OkfServer::new` (which only the session factory calls), not by an `impl Drop`. `catalog_label` returns `none` for a name no loaded catalog bears. `TOOL_SPAN` is private to `src/mcp/mod.rs`.
> - **Test helpers:** `telemetry::subscriber::capture::Observer` (`start`, `finish`) is `observe` with a scope that spans `.await`; `mcp::testing::McpProbe` is a minimal MCP client for a real session over `http::serve`. Tests live in `src/http/telemetry_tests.rs` and `src/mcp/telemetry_tests.rs`. A console session is not an MCP credential, so its test covers the HTTP span only. `search` takes no `catalog` argument; the MCP span tests use `get_concept`.
> - **Corrected (issue #464, review of PR #488, 2026-11-16).** `link_from` parses with `extract_with_context(&Context::new(), ..)`: `extract` starts from `Context::current()`, which with context activation on is the local active span, so a missing or malformed header would return that span as a fabricated remote link. **Task 6's `inject` has the mirror rule:** inject the context of the span the request is made under (`tracing::Span::current().context()` from `OpenTelemetrySpanExt`), not a bare `Context::current()`, and test it under an active span at both the `info` and `trace` filters (a trace-level helper span must not replace the caller's).
> - Task 10 Step 2a: the four named tests are in those two files (`FILTERS` lists the two span filters).

---

### Task 4: Publish, changesets, the poll loop and caches

**Files:** Modify `src/store/publish.rs`, `src/mcp/changesets.rs`, `src/app.rs`, `src/telemetry/metrics.rs`

- [ ] **Step 1: Failing tests**

Each uses `app_with_metrics` plus the in-memory span exporter:
- **`publish_records_duration_and_lock_wait`.** A publish produces:
  - spans `okf.publish` > `okf.publish.lock`, `okf.publish.lint` and `okf.publish.write`;
  - `okf_publish_duration_seconds_count{catalog="main",outcome="ok"} 1`;
  - one `okf_publish_lock_wait_seconds` observation.
- **`a_lint_failure_counts_by_rule`.** A publish refused with `LintFailed` gives `okf_lint_rejections_total{catalog,rule}` per error rule and `outcome="error"`.
- **`each_poll_tick_is_its_own_trace`.** Two `app.poll()` calls give two root `okf.poll` spans with different trace ids.
- **`catalog_gauges_follow_the_snapshot`.** `okf_catalog_concepts{catalog="main"}` equals the snapshot's concept count, and `okf_catalog_generation` its generation. After a delete and a poll, the deleted catalog's series is gone.
- **`open_changesets_gauge`.** It counts open changesets per catalog (a `SELECT catalog, count(*)` read in the callback is too slow for a scrape, so count from the poll: see Step 2).

- [ ] **Step 2: Implement**

- **`publish`.** `#[tracing::instrument(level = "info", name = "okf.publish", skip_all, fields(okf.catalog = %state.name(), okf.changeset.entries = tracing::field::Empty))]`. Child spans come from `info_span!` around:
  - the lock wait (`:150-154`), whose elapsed time goes to `okf.publish.lock_wait`;
  - `check` (lint);
  - the write and commit.

  `Metrics::publish(catalog, outcome, elapsed)`, and `Metrics::lint_rejection(catalog, rule)` for each error in `LintFailed` (rule names are the lint's own closed set).
- **The poll loop.** (Corrected, issue #465: the root is made in `App::poll`, see the note below.) Each tick runs `self.poll().instrument(tracing::info_span!(parent: None, "okf.poll"))`. `parent: None` makes each tick its own trace (spec "Background roots"). Inside `poll`:
  - each catalog refresh is `info_span!("okf.catalog.refresh", okf.catalog = %name)`;
  - the key-cache reload is `info_span!("okf.keycache.reload")`.

  Metrics: `okf.poll.duration{outcome}`, `okf.catalog.refreshes{catalog, result}` (`result` is `unchanged | reloaded | loaded | failed`), `okf.keycache.reloads{outcome}`.
- **Gauges.** Observable gauges over a `Weak<App>` (no strong cycle):
  - `okf.catalog.concepts{catalog}` and `okf.catalog.generation{catalog}` read `app.catalogs()`;
  - `okf.changesets.open{catalog}` reads a count `poll` stores (a `std::sync::RwLock<BTreeMap<String, i64>>` on `App`, filled by one grouped count per poll over the changesets table; take its table and column names from `migrations/`). A scrape callback never queries Postgres.

  `App::load_with_metrics` registers them after building `Arc<Self>`, through `Arc::downgrade`. Keep the returned `ObservableGauge` handles on `App` (`Mutex<Vec<..>>`) so their callbacks live as long as it does, and test that a gauge still reports after a poll.

- [ ] **Step 3: Gates and commit**

`feat(telemetry): publish, poll and catalog instrumentation (M15 Task 4)`

> **Corrected (issue #465, 2026-11-16).** What shipped differs from the sketch above in these ways; read `src/store/publish.rs`, `src/app.rs` and `src/telemetry/metrics.rs`.
> - **The `okf.poll` root is made inside `App::poll`**, not in `poll_until`: `poll` wraps a private `poll_pass` in `info_span!(parent: None, "okf.poll")` and records `okf.poll.duration` there, so a direct `app.poll()` is its own trace too (that is what `each_poll_tick_is_its_own_trace` calls). `poll_until` is unchanged.
> - **`publish` is a thin wrapper** (the `okf.publish` span, `okf.publish.duration`, one `okf.lint.rejections` per `LintFailed` error) around a private `publish_locked`. `okf.publish.lock` covers the in-process mutex, the transaction begin and the three row locks, and `okf.publish.lock_wait` is recorded even when a lock step refuses; `okf.publish.lint` wraps `check`; `okf.publish.write` wraps the writes, the generation bump, the audit entry and the commit. `okf.refresh.result` is a field on each `okf.catalog.refresh` span.
> - **`rule` is folded by `metrics::lint_rule_label`**: the lint's fixed ids pass, every `secret:<id>` becomes `secret`, anything else is `other`. Add a new lint rule to `LINT_RULES` (`every_rule_the_lint_names_is_a_label_of_its_own` scans the lint's source and fails until you do).
> - **Gauges:** `Metrics` keeps its `Meter` and `Metrics::observe_catalogs(Weak<App>)` registers `okf.catalog.concepts`, `okf.catalog.generation` and `okf.changesets.open`; `App::load_with_metrics` calls it after `Arc::new`. The gauge handles are **not** kept on `App` (see the Task 2 correction). `okf.changesets.open` reads `App::open_changesets`, filled by `changesets::open_counts` at load and once per poll pass; a catalog with none reports 0. Task 5's mail-queue gauge (it reads a count the poll stores, as the open-changeset one does) and Task 6's pool gauges can follow the same shape (a `Metrics` method taking a `Weak<App>`).
> - **`RefreshResult`** (`unchanged | reloaded | loaded | failed`) is the typed label; `okf.keycache.reloads` counts only an attempted reload (after `admin_generation` moved).
> - **Tests** live in `mcp::changesets::publish_tests`; `telemetry::subscriber::capture::SpanRecorder` runs a future under an in-memory span exporter via `WithSubscriber` (a thread-local default misses a multi-thread runtime's workers).

---

### Task 5: Sign-in, OAuth, rate limits and mail

**Files:** Modify `src/http/signin.rs`, `src/store/sign_in_events.rs` (or the two choke points only), `src/oauth/token.rs`, `src/oauth/ratelimit.rs`, `src/http/limits.rs`, `src/mail/queue.rs`, `src/telemetry/metrics.rs`

- [ ] **Step 1: Failing tests**

- **`sign_in_outcomes_are_counted_by_method`.** One password success, one failure and one lockout give `okf_auth_attempts_total{method="password",outcome="ok|failed|locked"}`. No attribute anywhere holds the email or the password.
- **`oauth_tokens_count_by_grant`.** An authorization-code exchange and a refresh give `okf_oauth_tokens_issued_total{grant="authorization_code"|"refresh_token"}`.
- **`rate_limit_refusals_count_by_limiter`.** Exhausting `sign_in_per_ip` gives `okf_ratelimit_rejections_total{limiter="sign_in_per_ip"}`. Exhausting the token bucket gives `limiter="oauth_token"`.
- **`each_mail_job_is_its_own_trace`.** A job processed by `send_mail` is a root `okf.mail.job` span with `okf.mail.template` and `okf.outcome`, and no address anywhere in it.
- **`mail_jobs_count_by_provider_and_outcome`.**
- **`mail_queue_depth_gauge`** reads `apalis.jobs` grouped by status, the count `poll` stores (as Task 4's changesets).

- [ ] **Step 2: Implement**

- **Sign-in.** `Metrics::auth_attempt(method, outcome)` at `signin::start_in` (`Ok`) and `signin::audit` (refusals), mapping the existing `Method`/`Outcome` enums to `&'static str` (no `_` arm).
- **OAuth.** `#[instrument(level = "info", name = "okf.oauth.token", skip_all, fields(okf.oauth.grant))]` on `token`. Count in `exchange_code` and `refresh` on success.
- **Rate limits.** Each refusal site (listed in Verified facts) records `app.metrics().rate_limited(Limiter::X)`, where `Limiter` is a closed enum: `SignInPerIp`, `ForgotPerEmail`, `ForgotPerIp`, `LinkStarts`, `OauthRegister`, `OauthToken`, `OauthConsent`. Its `as_str` gives the snake-case label. The limiters themselves are unchanged.
- **Mail.** In `send_mail`:

  ```rust
  let span = tracing::info_span!(parent: None, "okf.mail.job", okf.mail.template = %job.template.label(), okf.outcome = tracing::field::Empty);
  ```

  `template.label()` is a closed `&'static str`; add it if missing. Run the body inside the span with `.instrument(span)`. `Metrics::mail_job(provider, outcome)`, where `outcome` is `sent | retry | failed`. The provider label is `log | smtp | postmark | ses`, from the configured `MailProvider`.

- [ ] **Step 3: Gates and commit**

`feat(telemetry): sign-in, OAuth, rate-limit and mail instrumentation (M15 Task 5)`

> **Corrected (issue #466, 2026-11-15).** What shipped differs from the sketches above in these ways; read `src/telemetry/metrics.rs` and the sites for the code.
> - **The closed enums live in `telemetry::metrics`**: `Limiter`, `Grant`, `MailOutcome` and `MailStatus`, each with an `as_str`. `Metrics::auth_attempt(method, outcome)` takes the store's own `sign_in_events::{Method, Outcome}` (their `as_str` has no `_` arm), not `&'static str`. `Metrics::mail_job` takes the config's `MailProvider`; `Metrics::oauth_token_issued(Grant)` and `Metrics::rate_limited(Limiter)` are as sketched.
> - **`auth_attempt` for `Ok` is recorded at the end of `signin::start_in`**, after the session and event rows are written (a failure before then is the caller's refusal, which `audit` counts); refusals are counted in `signin::audit`.
> - **The token span is `#[instrument(name = "okf.oauth.token")]` on `token`**, with `okf.oauth.grant` recorded once the grant is read (an unsupported grant type stays empty). Tokens are counted at the end of `exchange_code` (after the replay check) and `refresh` (after the commit). The `/oauth/revoke` refusal also counts as `oauth_token`: it shares the bucket.
> - **`Limiter::SignInPerIp` is recorded at all three sites that take `sign_in_per_ip`**: `/signin`, the passkey routes (`passkeys::screen`) and the OIDC start (`http/oidc.rs`); `LinkStarts` is the signed-in link start (`http/oidc.rs`, `may_start_link`).
> - **`send_mail` is `#[instrument(name = "okf.mail.job", parent = None, ...)]`**, not an `info_span!` plus `.instrument`; the body moved to `deliver`, which returns the handler's answer and a `MailOutcome`. The span carries `okf.mail.template` (the stored template name, from `Template::name`, so `label()` was not added), `okf.mail.provider` and `okf.outcome`. `MailContext` gained `metrics` and `provider`.
> - **The mail queue gauge is held by `Metrics`**, not by `App`: `okf.mail.queue.depth` (`okf_mail_queue_depth`, an `i64` observable gauge, one series per `MailStatus` from the first scrape, so it is in `Metrics::FAMILIES`). `mail::queue::record_depth(pool, metrics)` does one grouped count of `apalis.jobs` (the same status `CASE` as `queue::list`, now shared as `status_sql!`) and `App::poll` calls it. Done jobs are not counted. Task 4's changeset gauge is independent of it.
> - **Test helpers:** `mcp::testing::{local_metrics, sample_of}`, `http::api::testing::login_app_with_metrics` and `oauth::testing::oauth_app_with_metrics`.

---

### Task 6: Postgres pool, clock check, archives and outbound propagation

**Files:** Modify `src/app.rs` (gauges), `src/clock_check.rs`, `src/http/archives.rs`, `src/oidc.rs`, `src/oauth/clients.rs`, `src/mail/postmark.rs`, `src/telemetry/propagation.rs`, `src/telemetry/metrics.rs`

- [ ] **Step 1: Failing tests**

- **`pool_gauges`.** `db_client_connection_count{state="idle"}` and `{state="used"}` sum to `pool.size()`.
- **`each_clock_check_is_its_own_trace`**, plus `okf_clock_offset_seconds` (an `f64` gauge from `offset_ms / 1000.0`) and `okf_clock_checks_total{outcome}`.
- **`archive_export_and_import_are_measured`.** `okf_archive_size_bytes_total{direction="export",catalog="main"}` equals the body length, and there is a `okf_archive_duration_seconds` observation per direction.
- **`outbound_requests_carry_traceparent`.** Against a local axum stub that echoes `traceparent`, an OIDC discovery fetch made inside a span sends a `traceparent` whose trace id is that span's.

- [ ] **Step 2: Implement**

- **Pool gauges.** Observable gauges over `Weak<App>`, from `pool.size()` and `pool.num_idle()`.
- **Clock checks.** `check_clock_until` runs each check in `info_span!(parent: None, "okf.clock.check")`.
- **Archives.** `export_catalog` and `import_catalog` get `#[instrument(level = "info", skip_all, fields(okf.catalog = tracing::field::Empty, okf.archive.size = tracing::field::Empty))]`. `name` comes straight from `Path<String>` and is evaluated before any check, so it is **not** an attribute up front. The handler records `okf.catalog` only after the catalog is resolved and admitted (the loaded `CatalogState`'s name), and a refused or unknown name never reaches the span or the archive metrics (those take the resolved name too) (review of PR #459). The size is recorded as `i64`. Test: export and import of an unknown catalog whose path segment holds the sentinel record no `okf.catalog` and no metric series; Task 7's sentinel test gains the same case for both routes.

  The same rule holds everywhere: **a span field or metric label taken from a path segment, query or body is recorded only after it has been validated against server state** (a loaded catalog, an existing user id). Task 8's policy doc states it.
- **`propagation::inject(headers: &mut reqwest::header::HeaderMap)`** is the probe's `inject`. reqwest 0.13's `HeaderMap` is http 1.x's.
- **Outbound clients.** Every outbound request goes through it: the OIDC client (`oidc.rs:276`; its requests are built in the openidconnect adapter, so inject there), CIMD (`oauth/clients.rs:635`) and Postmark (`mail/postmark.rs:80`). The investigator client is `okf-maintenance`'s and is out of scope.

- [ ] **Step 3: Gates and commit**

`feat(telemetry): pool, clock, archive metrics and outbound trace context (M15 Task 6)`

---

### Task 7: The mock OTLP collector and the privacy sentinel test

**Files:** Create `tests/support/otlp_collector.rs`, `tests/telemetry_privacy.rs`; modify `tests/support/mod.rs`, and `tests/support/server.rs` (an option to pass a `[telemetry]` config and env)

- [ ] **Step 1: The collector**

An axum server on `127.0.0.1:0` with `POST /v1/traces`, `/v1/logs` and `/v1/metrics`.
- **It decodes the bodies** with `opentelemetry-proto` (dev-dependency, `features = ["gen-tonic-messages", "trace", "logs", "metrics"]`, default features off; check it builds without tonic's transport) and `prost::Message::decode`.
- **It keeps** every request's headers and decoded payload.
- **It can be told** to answer 500 or to redirect (307 to another collector).
- **API:**
  - `Collector::start()`;
  - `collector.url()`;
  - `collector.wait_for(|requests| ..., Duration)`, which polls a condition and never sleeps blindly.

- [ ] **Step 2: Collector tests** (`tests/telemetry_privacy.rs`)

- **`configured_headers_arrive`.** `headers_env` holds `authorization=Bearer sentinel-otlp-token`. The collector sees that header on `/v1/traces`.
- **`a_sentry_dsn_derives_path_and_header`.** The collector, mounted at `/api/42/integration/otlp/v1/traces`, sees `x-sentry-auth: sentry sentry_key=public`.
- **`a_redirect_is_not_followed`.** The first collector answers 307 to a second. The second never receives anything, and `okf_telemetry_export_failures_total{signal="traces"}` rises.
- **`a_500_counts_as_a_failure`.**

- [ ] **Step 3: The sentinel test**

Start a `TestServer` with OTLP to the collector, traces and logs at `trace`, and `export_interval = "1s"`.

Plant `SENTINEL = "zq-sentinel-7f3a"` variants:
- in a bearer key's secret (create a key, then also send a wrong `Bearer zq-sentinel-7f3a-bad`);
- a cookie;
- a query string (`?token=zq-sentinel-7f3a-q`);
- a path segment (`/api/v1/users/zq-sentinel-7f3a-path`);
- a JSON body (a sign-in password);
- a concept's text (publish one containing it);
- an archive import (a document containing it);
- the catalog segment of both archive routes (`/catalogs/zq-sentinel-7f3a-cat/export` and `/import`), which must be refused without the name reaching any span or label.

Exercise:
- MCP: `search` for the sentinel, `create_concept`, `publish`;
- the REST API;
- the sign-in form;
- import and export;
- Task 1b's sites: a sign-up invitation through the `log` mail provider, a refused OAuth client whose id holds the sentinel, and an internal error whose text holds it (a test hook in `tests/support`, or a store failure the test can force).

Then establish that everything was exported before inspecting anything (review of PR #459: a quiet collector also means "never wired", and the metric reader would otherwise wait up to its interval):
1. Scrape `/metrics` first and keep the body.
2. Stop the server through its shutdown token, so `Telemetry::shutdown` flushes all three providers. `TestServer` exposes the result, a `ShutdownReport` that `Telemetry::shutdown` returns (Task 1 makes it return one, naming each provider's outcome). Assert that every provider flushed `Ok`, with none abandoned at the deadline.
3. **Positive assertions** that the records which could carry a leak arrived:
   - a span for every route exercised (by `http.route`);
   - an `mcp search` span;
   - an `okf.publish` span;
   - at least one log record from each exercised path;
   - metric points for `http.server.request.duration`, `okf.mcp.tool.duration` and `okf.publish.duration`.

   If any is missing, the test fails as "not exported", never as passed.
4. Assert that no log record or span came from a dependency target (`sqlx`, `apalis`, `rmcp`, `tower_http`, ...), and that no log body or attribute contains SQL keywords followed by a table name from `migrations/` (`SELECT`, `INSERT INTO`, `UPDATE`, `DELETE FROM`, case-insensitive).
5. Only then assert that no decoded span name, attribute value, event, link attribute, log body or log attribute, and no metric attribute, contains `zq-sentinel-7f3a`, and that the scraped `/metrics` body does not either.

**Revert check:** temporarily record `url.full` in `request_span`. The test must fail. Say so in the PR.

- [ ] **Step 4: Gates and commit**

`test(telemetry): mock OTLP collector and privacy sentinel (M15 Task 7)`

---

### Task 8: The dylint policy lint (warn) and the `dylint` gate

**Files:** Modify `Cargo.toml`, `justfile`, `tools/agentic/gate.sh`, `tools/agentic/README.md`, `tools/agentic/brief-common.md`, `AGENTS.md`; create `docs/contributing/policy-lints.md`

- [ ] **Step 1: `Cargo.toml`**

```toml
# The tracing policy lint (docs/contributing/policy-lints.md): every non-test
# function carries `#[tracing::instrument]`. Only the `hyp_tracing` library
# is loaded (M15 Decision 2).
[workspace.metadata.dylint]
libraries = [
  { git = "https://github.com/hypotheosis/hyp-rust-policy-lints", tag = "v0.1.2", pattern = "lints/hyp_tracing" },
]

# `cfg_attr(dylint_lib = "hyp_tracing", allow(...))` exemptions name a cfg
# only dylint sets; this keeps ordinary builds from warning about it.
[lints.rust.unexpected_cfgs]
level = "warn"
check-cfg = ["cfg(dylint_lib, values(any()))"]
```

`[workspace.metadata.dylint]` needs a `[workspace]` table, so add one at the end of `Cargo.toml`:

```toml
[workspace]
exclude = [".worktrees"]
```

> **Corrected (issue #469, 2026-11-15).** Verified on the real crate:
> - dylint reads only `[workspace.metadata.dylint]`. With the same libraries under `[package.metadata.dylint]`, `cargo dylint list --all` prints "No libraries were found". The empty `[workspace]` table is required.
> - `exclude = [".worktrees"]` is required too. This repository keeps git worktrees under `.worktrees/`, nested in the checkout. Without `exclude`, a worktree whose `Cargo.toml` predates the `[workspace]` table fails every cargo command with "current package believes it's in a workspace when it's not", so every open branch would need a merge of `main` first.
> - A single-package workspace changes nothing else: `Cargo.lock` is unchanged, `target/` stays at the root, the repository has no `.sqlx/` directory (the `justfile` exports `SQLX_OFFLINE=true` for apalis' shipped cache), and `fmt`, `lint` and `test` pass as before.

- [ ] **Step 2: `just dylint` and the gate**

```make
# The dylint policy lints (docs/contributing/policy-lints.md). Needs
# `cargo install --locked cargo-dylint dylint-link`; the first run builds the
# lint library on its own pinned nightly (minutes). `--all-targets` so test
# code is compiled and excluded properly.
dylint:
    DYLINT_RUSTFLAGS="-W uninstrumented_fn" cargo dylint --all -- --all-targets
```

In `gate.sh`, add `dylint) limit=${AGENTIC_TIMEOUT_DYLINT:-45m}; locked=1; cmd=(just dylint "$@") ;;` and list it in the usage text and the header comment's table. Update `tools/agentic/README.md`, `brief-common.md` ("Gates") and `AGENTS.md`'s table: `gate.sh dylint` is required for every task from M15 Task 8 on.

> **Corrected (issue #469, 2026-11-15).** Verified: the lint is deny by default (`cargo dylint list --all` shows `deny`; a run without the flag fails with 1798 errors in the library alone). `DYLINT_RUSTFLAGS="-W uninstrumented_fn"` makes every one a warning, and `gate.sh dylint` passes. **The flag is not part of cargo's fingerprint**, so a cached run replays the old level's diagnostics: Task 10 must `touch src/lib.rs src/main.rs src/bin/okf-maintenance.rs` after changing `-W` to `-D`, or the gate would keep passing on replayed warnings. `docs/contributing/policy-lints.md` says so.

- [ ] **Step 3: `docs/contributing/policy-lints.md`**

Write the spec's "The instrument policy" as the contributor doc:
- level rules;
- `skip_all` always, with `fields(...)` for safe ids;
- never `err` or `ret`;
- `#[tool]` methods: the rmcp-generated functions need nothing, and the tools themselves take `#[instrument(level = "debug", skip_all, ...)]` below `#[tool]` (Task 3's `call_tool` is the `info` span);
- exemptions with `cfg_attr(dylint_lib = "hyp_tracing", allow(uninstrumented_fn, reason = "..."))` and the accepted reasons list;
- how to run (`gate.sh dylint`), and that `cargo dylint list --all` must list the lint, or nothing was checked.

- [ ] **Step 4: Verify**

`tools/agentic/gate.sh dylint` passes, with warnings. Record the warning count in the PR: it is the sweep's size.

> **Corrected (issue #469, 2026-11-15).** Measured on `2a879d4` plus this change: **1811 warnings**, 1798 in the library (`src/`, test code excluded), 1 in `okf-mcp` (`src/main.rs`) and 12 in `okf-maintenance` (`src/bin/okf-maintenance.rs`). The first run on a warm machine took about 70 s.

- [ ] **Step 5: Commit**

`chore(lint): dylint uninstrumented_fn policy lint in warn mode (M15 Task 8)`

---

### Tasks 9a-9e: The `#[instrument]` sweep

Each sub-task is its own PR and covers only its module group (the File map). The procedure is the same for each:

1. **Run** `tools/agentic/gate.sh dylint 2>&1 | tee target/i<N>-logs/dylint.log`, and list the `uninstrumented_fn` warnings in your paths.
2. **Annotate each function** following `docs/contributing/policy-lints.md`:
   - `#[tracing::instrument(level = "trace", skip_all)]` for private helpers;
   - `level = "debug"` for `pub`/`pub(crate)` entry points;
   - `fields(...)` only for safe ids: catalog name, concept count, generation, `user_id`, `key_id`, enum discriminants, sizes as `i64`;
   - **never** a path, a document, an email, a name, a token, SQL or an error message.
   - `#[tracing::instrument]` on an `async fn` instruments its future, so no manual `.instrument` is needed.
3. **Exempt** with a reason from the accepted list where a span would be wrong or costly:
   - `Display`/`Debug` impls and their helpers;
   - per-byte predicates;
   - trivial accessors, grouped in one exempt `impl`;
   - one-line delegations;
   - code that runs before the subscriber exists;
   - `src/telemetry/` trait impls;
   - span makers and code that captures the enclosing span: `request_span` and the `on_response` hook (Task 3), and `capture_request_span`. Their own span would become the request's parent, or be captured instead.

   Anything else goes in the PR description for review.
   > **Corrected (issue #471, 2026-11-15).** Accessors, one-line delegations and trivial constructors are exempted one function at a time (`cfg_attr(dylint_lib = "hyp_tracing", allow(uninstrumented_fn, reason = "..."))` on the `fn`), not by moving them into a separate exempt `impl`; this keeps the diff free of code moves. The reason strings in `src/okf/**`, `src/overlay.rs` and `src/selection.rs` are the pattern to reuse.
   > **Corrected (issue #471, 2026-11-15).** A span opened inside a rayon closure is a root span: workers do not inherit the thread-local current span or a scoped subscriber. Every `par_iter` in a swept module therefore captures the caller with `crate::okf::caller::Caller::capture()` before the iterator and uses `map_init(|| caller.enter(), ...)`. Later sweeps with parallel passes (9a, 9d) should reuse or generalise that helper rather than write their own.
4. **Benchmark-sensitive paths** (`okf` parsing and analysis, `rayon` loops): a `trace`-level disabled span is one atomic load. Exempt only an inner per-element function, and say which in the PR.
5. **No behaviour changes.** `fmt`, `lint`, `test` and `dylint` pass. The `dylint` warnings for your paths are zero.
6. **Run the sentinel test if it exists on `main`** (`tools/agentic/gate.sh narrow --test telemetry_privacy`; Task 7 creates it). 9b and 9e may start before Task 7 merges. A sweep PR that merges first says "sentinel not yet available" in its report, and Task 10 runs the sentinel over the whole swept tree. A careless `fields(...)` is what it catches.

| Task | Paths | Needs |
|---|---|---|
| 9a | `src/store/**` | 8, 4, 6 |
| 9b | `src/okf/**`, `src/overlay.rs`, `src/render.rs`, `src/selection.rs` | 8 |
| 9c | `src/http/**`, `src/oauth/**`, `src/oidc.rs`, `src/oidc/**` | 8, 3, 5, 6 |
| 9d | `src/mcp/**`, `src/auth.rs`, `src/mail/**`, `src/cli.rs`, `src/cli/**`, `src/app.rs`, `src/catalog.rs`, `src/config.rs`, `src/archive.rs`, `src/db.rs`, `src/clock*.rs`, `src/ntp.rs`, `src/keys.rs`, `src/keyed_lock.rs`, `src/error.rs`, `src/write.rs`, `src/main.rs`, `src/telemetry/**` (exemptions mostly) | 8, 3, 4, 5 |
| 9e | `src/investigator/**`, `src/bin/**`, `src/maintenance/**` | 8 |

**9e note:** `okf-maintenance`'s `quiet_logging` filter passes only the rejection target, so these spans are recorded nowhere. Instrument for the lint, with the same `skip_all` rule. Evidence and tokens flow through these functions, and a future OTLP export (the deferred follow-up) must not need another privacy pass.

> **Corrected (issue #474, 2026-11-15).** A `#[schemars(schema_with = "...")]` field makes the `JsonSchema` derive emit a wrapper function that no attribute on the struct or the field reaches (the lint flags the attribute's string). Exempt it by putting the type in a small inline module that carries the `allow`, and re-export the type (`maintenance::types::Submission` does this). 9c and 9d may meet the same pattern in `http/` and `mcp/` types.

Commit: `chore(tracing): instrument <group> (M15 Task 9x)`.

---

### Task 10: The lint at deny

**Files:** Modify `justfile` (`-W` becomes `-D`), `docs/contributing/policy-lints.md`, `tools/agentic/brief-common.md`

- [ ] **Step 1:** Merge `main`, change `-W` to `-D` in the `justfile`, `touch src/lib.rs src/main.rs src/bin/okf-maintenance.rs` (cargo does not see a lint-level change, Task 8), and run `gate.sh dylint`. Fix any stragglers (functions added since the sweep), in their files.
- [ ] **Step 2:** Run `gate.sh dylint` with `-D` and see it pass. Then add a function without `#[instrument]` and see it fail, and remove it again. Say so in the PR.
- [ ] **Step 2a:** Re-run Task 3's root-span tests with the span filter at `trace` over the swept tree: `a_forged_traceparent_is_a_link_not_the_parent` (parent none), `an_unmatched_path_makes_no_span` (nothing exported at all), `principal_ids_land_on_the_request_span_under_helper_spans` and `error_kind_lands_on_the_tool_span_under_helper_spans`. Task 3 already writes them to take the filter as a parameter and run at both `info` and `trace`.
- [ ] **Step 2b:** Run the sentinel test over the swept tree, `tools/agentic/gate.sh narrow --test telemetry_privacy`, with `traces_filter` and `logs_filter` at `trace` (the test's own config), so every `trace`-level span the sweep added is exported and checked. Fix any field it catches in the file that added it.
- [ ] **Step 3: Commit** `chore(lint): uninstrumented_fn at deny (M15 Task 10)`

---

### Task 11: Operations docs, examples, close-out

**Files:** Modify `docs/operations.md`, `README.md`; create `examples/kubernetes/metrics-service.yaml`, `examples/kubernetes/servicemonitor.yaml`, `examples/grafana/okf-mcp.json`, `tests/grafana_dashboard.rs`, `changelog/<ts>-m15-milestone.md`

- [ ] **Step 1: `docs/operations.md`**

- **Section 3:** add `[telemetry]` rows to the key table (`:72-110`) and a `[telemetry]` block to the TOML example (`:138-184`). Add `RUST_LOG`'s new meaning (stderr only) and the `OTEL_*` variables to "The environment" (`:196-213`).
- **A new "Telemetry" section after section 5**, covering:
  - OTLP to a receiver, to Sentry (DSN), and through an OpenTelemetry Collector;
  - the metrics listener and why it is separate;
  - tuning stderr down and reading the receiver instead;
  - what is never exported, and that stderr is the one exception: `okf_mcp::local_only` events carry caller text, full errors and (with the `log` mail provider) whole messages, so stderr stays in the cluster or is filtered on that target before shipping;
  - the metric reference (every family, its labels, its meaning) and the span reference (names, attributes).
- **Section 5's YAML:**
  - the Deployment gains `containerPort: 9464` (named `metrics`);
  - env `OKF_OTLP_HEADERS` from a Secret;
  - `OTEL_RESOURCE_ATTRIBUTES` built from the downward API (`k8s.pod.name=$(POD_NAME),k8s.namespace.name=$(POD_NAMESPACE)`).

  Point to the new example files for the Service and ServiceMonitor. The Ingress is unchanged and must not route `9464`.

- [ ] **Step 2: Examples and the dashboard test**

- `examples/kubernetes/metrics-service.yaml`: a ClusterIP Service on port `metrics`.
- `examples/kubernetes/servicemonitor.yaml`: selects it, path `/metrics`, interval 30 s. A comment says the label your Prometheus selects ServiceMonitors by (often `release: <name>`) must be added.
- `examples/grafana/okf-mcp.json`: request rate and p95 by route; tool calls and p95 by tool; publish duration; poll duration and failures; auth attempts by outcome; mail jobs; export failures; clock offset.
- `tests/grafana_dashboard.rs`: every metric name in the dashboard's `expr` fields is in `Metrics::FAMILIES` (or the full list Task 2 exposes), so a rename breaks the test, not the dashboard.

- [ ] **Step 3: Follow-up issue**

File the deferred `okf-maintenance` OTLP export as an issue (`enhancement`, `milestone-15` follow-up), linking the spec's Non-goals.

- [ ] **Step 4: Smoke test**

Build the image locally (or `cargo run -- serve`) with `[telemetry]` pointed at a local OpenTelemetry Collector container (`otel/opentelemetry-collector` with a debug exporter), plus `[telemetry.prometheus]`. Then:
- make one MCP call and one console sign-in;
- see the spans and logs in the collector's output, with `trace_id` matching stderr's JSON line;
- `curl` `/metrics`;
- stop the server with SIGTERM and see the last batch flushed within 4 s.

Record the commands and trimmed output in the milestone changelog.

- [ ] **Step 5: README and changelog**

README gets a short "Observability" paragraph linking the docs. The milestone changelog summarises the tasks, the deviations and the follow-ups.

- [ ] **Step 6: Gates and commit**

`docs: observability operations, examples and M15 close-out (M15 Task 11)`

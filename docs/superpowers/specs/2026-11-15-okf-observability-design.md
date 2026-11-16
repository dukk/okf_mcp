# okf-mcp observability: design

Status: designed 2026-11-15 (issue #425); delivered in milestone 15 (epic #460), closed out 2026-11-16 (issue #497).

## Goal

The server tells its operator what it is doing, without anyone reading its
logs by hand:

- **Metrics** on a Prometheus `/metrics` endpoint for scraping, and the
  same metrics pushed over OTLP.
- **Traces** over OTLP: one trace per HTTP request, MCP tool call, poll tick
  and mail job, with every function in the crate able to appear as a span.
- **Logs** over OTLP, correlated with traces, and on stderr as text or JSON.

The OTLP side works with any OTLP/HTTP receiver: Traceway, Sentry's OTLP
endpoint, or an OpenTelemetry Collector.

Today the server has none of this: `tracing_subscriber::fmt()` on stderr
(`src/main.rs`), one `tower_http` line per request, and `/healthz`.

## Non-goals

- **`okf-maintenance` export.** The runner shares the crate, so its
  functions are instrumented (see "The instrument policy"), but its fixed
  `Targets` filter (`src/bin/okf-maintenance.rs`, `quiet_logging`) stays
  as it is and exports nothing. OTLP for the runner is a follow-up issue,
  with its own privacy review: rmcp traces there can carry evidence and
  tokens.
- **CLI subcommands** (`migrate`, `import`, `export`, ...) keep plain
  stderr logging and export nothing.
- **The native Sentry SDK.** Sentry is reached through its OTLP endpoint.
  `sentry-tracing` would be one more subscriber layer, so adding it later
  changes nothing in this design; it is a follow-up if Sentry's issue
  grouping over OTLP proves too weak.
- **Dashboards or alert rules in a live system.** The repository ships an
  example dashboard and example scrape objects; installing them is the
  operator's.
- **Multiple replicas.** The server stays single-replica.

## Decisions

1. **OTel SDK metrics are the only metrics API.** One `SdkMeterProvider`
   with two readers: a `PeriodicReader` exporting OTLP, and the
   `opentelemetry-prometheus` reader that `/metrics` renders. Each
   instrument is defined once. Cost, accepted: every `opentelemetry*` crate
   is pinned at the release `opentelemetry-prometheus` supports (0.33 at
   the time of writing), and `opentelemetry_sdk`'s
   `experimental_metrics_custom_reader` feature is on, because that is what
   lets the Prometheus exporter mount as a reader. The Cargo.toml comment
   says so. Rejected: `prometheus-client` for `/metrics` (metrics would
   not reach OTLP without a collector scraping them), the `metrics` facade
   (weak OTLP bridge, a second naming scheme), and two pipelines (every
   metric recorded twice).
2. **`/metrics` gets its own listener**, at `[telemetry.prometheus] listen`,
   off unless configured, with no authentication. Config validation
   rejects an address that overlaps the main `listen`. The ingress never
   routes to it; Prometheus scrapes the pod's metrics port. Rejected: a
   route on the public listener, keyed (Prometheus then needs a credential,
   and the route is reachable from the internet) or open (it would leak
   catalog names and usage).
3. **One filter per layer, never one on the whole stack.** A filter on the
   registry is ANDed into every layer and decides which spans exist at all,
   so a quiet stderr would silence the exporters too. stderr and OTLP are
   tuned independently: an operator can drop stderr to `warn` and read the
   OTLP receiver instead.
4. **An incoming `traceparent` is a span link, never the parent.** The
   public listener is unauthenticated until the auth layer runs; adopting
   the header would let any caller choose the trace id and the sampling
   decision. Outgoing `reqwest` calls inject the current context.
5. **Privacy.** Metric labels come from closed sets only. Span attributes
   may also carry principal ids (user id, team id, key id), never names,
   email addresses, paths, document text, SQL, URIs, queries, headers or
   error text. See "Privacy".
6. **Every function is instrumented**, enforced by the `uninstrumented_fn`
   dylint policy lint, with `skip_all` on every `#[instrument]`. See "The
   instrument policy".
7. **Sentry over OTLP.** With `sentry_dsn_env` set and no `endpoint`, the
   OTLP target is derived from the DSN:
   `<scheme>://<host>[:port]<path>api/<project>/integration/otlp`, with the
   header `x-sentry-auth: sentry sentry_key=<public key>`.

## Architecture

A new module, `src/telemetry/`, owns the subscriber, the providers and the
metrics handle. The rest of the crate only uses `tracing` macros,
`#[instrument]` and the typed `Metrics` handle.

```
src/telemetry/
  mod.rs        Telemetry::start, the guard, shutdown
  config.rs     TelemetryConfig (the [telemetry] table) and its validation
  subscriber.rs build_subscriber: the layers and their filters (pure, testable)
  otlp.rs       the OTLP target (endpoint, headers, Sentry DSN), exporters
  metrics.rs    Metrics: the typed instruments, label enums, bucket constants
  scrape.rs     the /metrics listener and render
  propagation.rs  traceparent link (inbound), inject (outbound)
```

### Startup

`main` calls `Telemetry::start(&config.telemetry)` before the subscriber
exists. It builds the resource, the exporters and the providers; an
exporter that fails to build logs a warning (to stderr, through a fallback
subscriber) and the server runs without that export. Then
`build_subscriber` installs the registry, then the command runs, then
`telemetry.shutdown()`.

The CLI subcommands get only the stderr layer: `Telemetry::start` is called
for `serve` alone.

### The layers

`tracing_subscriber::registry()` with, each behind its own `.with_filter`:

| Layer | Filter | Notes |
|---|---|---|
| stderr `fmt` | `RUST_LOG`, else `[telemetry.stderr] filter`, else today's `warn,okf_mcp=info,tower_http=info` | `format = "text"` (default, today's output) or `"json"` (one object per line with `trace_id` and `span_id`) |
| OTLP spans (`tracing-opentelemetry`) | `[telemetry.otlp] traces_filter`, default `info` | Spans, plus `ERROR` events as span events (so the span's status is Error); other events go to the log layer only |
| OTLP logs (`opentelemetry-appender-tracing`) | `[telemetry.otlp] logs_filter`, default `info` | |

The OTLP layers take only this crate's own targets (`okf_mcp`, minus
`okf_mcp::local_only`): dependency events (sqlx's statement text, rmcp's
JSON-RPC bodies, apalis, openidconnect, the AWS SDK) are never exported.
*(Corrected, review of PR #459, round 3, 2026-11-15: was a deny-list.)*
stderr caps `rmcp` at `info` (at `debug` it logs whole
JSON-RPC bodies, credentials among them). The allow-list also excludes the
export stack's own targets (`opentelemetry`, `hyper`, `hyper_util`,
`reqwest`, `h2`), which reach stderr only. Without that, an export failure
becomes a log record that is exported, fails, and so on.

Each layer gets its own `EnvFilter`, cloned or built from the same
directive string (`EnvFilter` is `Clone` in tracing-subscriber 0.3.23).
Directive components are trimmed before parsing. *(Corrected, review of PR #459, 2026-11-15.)*

`global::set_tracer_provider` is set so outgoing propagation and any
library span find the provider; the providers are also held in the
`Telemetry` guard for shutdown.

### Export

OTLP/HTTP with protobuf (`http-proto`), sent with the async `reqwest` 0.13
the crate already uses, on `ring`: never `grpc-tonic` or `reqwest-rustls`
(each brings a second TLS stack). The batch span and log processors run on
the tokio runtime (`rt-tokio` and the `experimental_*_with_async_runtime`
features). The OTLP `reqwest::Client` follows no redirects, so the auth
headers are never sent to another host.

The endpoint is a base URL; the code appends `/v1/traces`, `/v1/metrics`
and `/v1/logs` (the SDK uses a URL given in code verbatim). Validation
rejects a non-http(s) scheme, a query or a fragment, and warns on a base
that already ends in `/v1/...`. The endpoint is logged with userinfo,
query and fragment removed.

Every exporter is wrapped in a counting exporter that adds failed batches
to `okf.telemetry.export.failures{signal, reason}`. That counter lives on
`/metrics`, which keeps working while the collector is down.

### Metrics provider

One `SdkMeterProvider`, built whenever `serve` runs (also with OTLP off,
so `/metrics` always works), with:

- the `PeriodicReader` over the OTLP metric exporter, present only when
  OTLP is configured, every `export_interval` (default 60 s);
- the `opentelemetry-prometheus` exporter as a reader, built
  `without_target_info()` and `scope_info_enabled(false)` *(Corrected, review of PR #459, 2026-11-15.)*, rendering into its
  own `prometheus::Registry`.

The global meter provider is set before any instrument is created, so no
no-op instrument is cached.

### The `Metrics` handle

`Metrics` is built once from the provider and kept on `App`. Callers use
typed methods (`metrics.tool_call(tool, catalog, outcome, elapsed)`), never
an instrument or a raw attribute. Label values are closed enums with an
`as_str`; free-form values (the MCP tool name, the route) are mapped
through an allowlist to `other`. Histograms in seconds have explicit
bucket boundaries in `metrics.rs` (the SDK defaults are scaled for
milliseconds) and `.with_unit("s")`.

### The `/metrics` listener

A second axum `Router` with one route, `GET /metrics`, on its own
`TcpListener`, served by the same `axum::serve` setup and the same
`CancellationToken` as the main listener. The body is the Prometheus text
format (`text/plain; version=0.0.4`). The handler encodes into a buffer
first and returns 500 on an encoding error, never a partial body.

### Shutdown

After the HTTP listeners have drained, `Telemetry::shutdown` flushes and
shuts down the tracer, logger and meter providers in parallel, each on a
plain `std::thread`, under one deadline of 4 s, then logs (to stderr) any
flush it abandoned. Plain threads because the tokio batch processor
ignores the timeout given to `shutdown_with_timeout`, and a dead collector
otherwise costs several retry timeouts. Shutdown is idempotent and
ignores `AlreadyShutdown`. The tracer is shut down before the logger, so
the logger still carries errors from the others.

### Propagation

- **Inbound.** The request span's maker extracts `traceparent`/`tracestate`
  with `TraceContextPropagator` and adds the extracted context as a link
  on the new root span, never as its parent (Decision 4).
- **Outbound.** OIDC discovery, token exchange, CIMD fetches, the Postmark
  mailer and any other `reqwest` call go through one helper that injects
  the current span's context into the request headers.
- **Background roots.** The poll tick, the clock check and each apalis
  mail job open their span with `parent: None`, so each is its own trace.
  A span exports only when it closes; a long-lived parent would make one
  trace that never ends.

## Configuration

A new `[telemetry]` table. Absent, the server behaves as today: stderr in
text, no OTLP, no metrics listener.

```toml
[telemetry]
service_name = "okf-mcp"          # default "okf-mcp"
environment = "production"        # deployment.environment.name; optional

[telemetry.stderr]
format = "text"                   # "text" | "json"
filter = "warn,okf_mcp=info"      # RUST_LOG overrides it

[telemetry.otlp]
endpoint = "https://otlp.example.com"   # base URL; /v1/<signal> is appended
headers_env = "OKF_OTLP_HEADERS"  # env var holding name=value,name=value
# sentry_dsn_env = "OKF_SENTRY_DSN"     # instead of endpoint + headers_env
traces_filter = "info"
logs_filter = "info"
sample_ratio = 1.0                # TraceIdRatioBased under ParentBased
export_interval = "60s"           # metrics push interval

[telemetry.prometheus]
listen = "0.0.0.0:9464"
```

Rules (checked at load, with the existing `Config::validate` style):

- `endpoint` and `headers_env` are set together or not at all (a receiver
  with no auth sets `headers_env` to a variable holding `none`).
  `sentry_dsn_env` excludes both. With none of the three, OTLP is off.
- The variables named by `headers_env` and `sentry_dsn_env` are read at
  startup into a `SecretString`; a missing variable fails startup and the
  error names the variable, never its value. Secrets never go in the file.
- `sample_ratio` is in `0.0..=1.0`; `export_interval` is more than zero;
  the filters must parse.
- `prometheus.listen` must not equal the main `listen`, and not share its
  port on an unspecified address.
- The standard `OTEL_*` environment variables (`OTEL_RESOURCE_ATTRIBUTES`,
  `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_TRACES_SAMPLER` when `sample_ratio`
  is unset, `OTEL_BSP_*`, `OTEL_METRIC_EXPORT_INTERVAL`) still apply as the
  SDK defines them. `OTEL_SDK_DISABLED=true` turns OTLP off.
  *(Corrected, issue #476, 2026-11-16: `OTEL_METRIC_EXPORT_INTERVAL` does not apply; `export_interval` is always passed to the reader and overrides it.)*
  `OTEL_EXPORTER_OTLP_HEADERS` is merged over the headers from
  `headers_env`, so a header named in both takes the environment's
  value (the SDK's behaviour). *(Corrected, M15 plan, 2026-11-15.)*

The resource carries `service.name`, `service.version` (the crate
version), `deployment.environment.name` when set, and whatever
`OTEL_RESOURCE_ATTRIBUTES` adds (the Kubernetes example fills
`k8s.pod.name` and `k8s.namespace.name` from the downward API).

## What is instrumented

### Spans

| Area | Span | Attributes |
|---|---|---|
| HTTP, every route | `TraceLayer` with a span maker: `otel.name = "<METHOD> <route>"`, `otel.kind = server` | `http.request.method` (unknown ones as `_OTHER`), `http.route` from `MatchedPath`, `http.response.status_code` (i64), `otel.status_code = ERROR` for 5xx only, `enduser.id` and `okf.key.id` once the principal is known (`Principal` has no team; corrected, M15 plan, 2026-11-15; `okf.key.id` is the bare word `session` for a console session, never `session:<user name>`; corrected, issue #464, 2026-11-16). An unmatched route gets no span. No URI, query or header |
| MCP tools (42) | one `info` span per call, `otel.name = "mcp <tool>"`, opened in `OkfServer::call_tool`, the single choke point (corrected, M15 plan, 2026-11-15: was `#[instrument]` on each tool, which now carry `debug` spans beneath it) | `okf.tool`, `okf.catalog` (a loaded catalog's name, else `none`), `okf.outcome`, `okf.error.kind`, `enduser.id`, `okf.key.id` (same rule as the HTTP row; corrected, issue #464, 2026-11-16) |
| Publish | `okf.publish`, children for the lock wait, lint, write, commit | `okf.catalog`, `okf.changeset.entries` |
| Poll loop | `okf.poll` root per tick; a child per catalog refresh and the key-cache reload | `okf.catalog`, `okf.refresh.result` |
| Mail | `okf.mail.job` root per apalis job | `okf.mail.provider`, outcome; never the address |
| Auth | sign-in, session, OIDC, passkey, OAuth authorize and token | `method`, `outcome`; no token, code, email or password (corrected, issue #497, 2026-11-16: carried by `signin::audit`, `signin::start_in`, `okf.oauth.token`, and OAuth authorize's `check_code` and `finish`, not one span per flow) |
| Postgres | the store functions' own `#[instrument]` spans, `db.system.name = postgresql`, `db.operation.name` from a constant (corrected, issue #497, 2026-11-16: `<module>.<function>`, on functions that take a pool, connection or transaction) | never SQL text or bind values |
| Clock check | `okf.clock.check` root | offset, outcome |
| Archives | export and import spans | `okf.catalog`, bytes, outcome |
| Everything else | the instrument policy below (`debug` or `trace`) | `fields(...)` of safe ids only |

### Metrics

OTel names; the Prometheus reader renders them with `_` and unit and
`_total` suffixes (`okf.mcp.tool.duration` becomes
`okf_mcp_tool_duration_seconds`).

| Instrument | Kind | Attributes |
|---|---|---|
| `http.server.request.duration` | histogram, s | `http.request.method`, `http.route`, `http.response.status_code` |
| `http.server.active_requests` | up-down counter | `http.request.method`, `http.route` |
| `okf.mcp.tool.duration` | histogram, s | `tool`, `catalog`, `outcome` |
| `okf.mcp.sessions.active` | up-down counter | |
| `okf.publish.duration` | histogram, s | `catalog`, `outcome` |
| `okf.publish.lock_wait` | histogram, s | `catalog` |
| `okf.lint.rejections` | counter | `catalog`, `rule` |
| `okf.changesets.open` | gauge | `catalog` |
| `okf.poll.duration` | histogram, s | `outcome` |
| `okf.catalog.refreshes` | counter | `catalog`, `result` |
| `okf.catalog.concepts` | gauge | `catalog` |
| `okf.catalog.generation` | gauge | `catalog` |
| `okf.keycache.reloads` | counter | `outcome` |
| `okf.auth.attempts` | counter | `method`, `outcome` |
| `okf.oauth.tokens.issued` | counter | `grant` |
| `okf.ratelimit.rejections` | counter | `limiter` |
| `okf.mail.jobs` | counter | `provider`, `outcome` |
| `okf.mail.queue.depth` | gauge | `status` |
| `db.client.connection.count` | gauge | `state` (`idle`, `used`) |
| `okf.clock.offset` | gauge, s | |
| `okf.clock.checks` | counter | `outcome` |
| `okf.archive.size` | counter, By (rendered `okf_archive_size_bytes_total`; corrected, M15 plan, 2026-11-15: `okf.archive.bytes` would render `_bytes_bytes_total`) | `direction`, `catalog` |
| `okf.archive.duration` | histogram, s | `direction`, `outcome` |
| `okf.telemetry.export.failures` | counter | `signal`, `reason` |
| `okf.build.info` | gauge (1) | `version` |

Gauges read existing state (the snapshot cache, the key cache, the pool,
`apalis.jobs`) through observable callbacks, so nothing new is stored
*(Corrected, issue #497, 2026-11-16: one thing is: migration 0018, the
partial index `changesets_open` on open changesets, keeps the open-changeset
count from scanning the whole table each poll pass.)*.
Catalog names are bounded by the catalogs that exist; a deleted catalog's
series stop at the next collection.

## Privacy

- **Metric attributes** come only from closed sets: route templates, the
  known tool names (anything else is `other`), catalog names, and outcome,
  method, rule, reason and status enums. Never a user, team, key, path or
  id.
- **Span attributes** may also carry principal ids: `enduser.id` (the
  user's id) and `okf.key.id`. *(Corrected, M15 plan, 2026-11-15.)* Never names, email
  addresses, display names or key material. *(Corrected, issue #464,
  2026-11-16.)* A console session's key id is `session:<user name>`, so for a
  session credential `okf.key.id` is the kind only, `session`; the user is
  identified by `enduser.id` alone. API keys and OAuth grants record their
  real key id. One helper (`auth::record_principal_ids`) applies the rule to
  both the HTTP and the MCP span.
- **Never exported** (no OTLP span, event, log record or metric, and
  nothing on `/metrics`): URIs, query strings, headers, request or
  response bodies, document or concept text, concept paths, SQL, bind
  values, error messages (only the error's kind, a closed enum),
  passwords, tokens, codes, cookies. *(Corrected, review of PR #459, 2026-11-15.)*
- **stderr is the one exception, and it is sensitive.** Events with
  target `okf_mcp::local_only` (below) reach stderr only, and may carry
  caller-supplied text, an error's full text and, with the `log` mail
  provider, whole messages with their setup and reset links, exactly as
  stderr does today. Operators treat the pod's stderr as they treat the
  server's own log: it stays in the cluster, it is not shipped to a
  third party, and a log pipeline that does ship it filters
  `okf_mcp::local_only` out (with the JSON format, the `target` field).
  The `log` mail provider is for development only. `docs/operations.md`
  says so (plan Task 11). *(Corrected, review of PR #459, 2026-11-15.)*
- **Existing events.** `skip_all` protects only arguments. An event that
  carries caller-supplied text, an error's full text or content (the `log`
  mail provider's message) logs it with target `okf_mcp::local_only`,
  which both OTLP layers turn off, so it reaches stderr only. The exported
  event keeps a closed value (`kind`). *(Corrected, review of PR #459,
  2026-11-15; plan Task 1b.)*
- **`#[instrument]`** always has `skip_all`; values are recorded only
  through `fields(...)`. `err` and `ret` are never used: `err` logs every
  `Err` at ERROR, which is wrong for validation failures, and records the
  error's text.
- **Enforcement.** The sentinel test (see "Testing") plants known secret
  strings in every place one can enter a request and fails if any reaches
  an exported span, log record or metric.

## The instrument policy

The `uninstrumented_fn` dylint lint fires on any non-test function without
`#[tracing::instrument]`, at deny level once the sweep is done. It comes
from the policy-lints library pinned by tag in the dylint metadata table
of `Cargo.toml`; `const fn`, closures and test-only code are out of its
scope. `docs/contributing/policy-lints.md` (new) carries the rules below.

- **Level.** Public entry points into a module: `debug`. Private helpers:
  `trace`. The HTTP request span, `mcp <tool>`, publish and the
  background roots: `info`; HTTP handlers and other public or async
  functions: `debug`, beneath those roots *(Corrected, issue #497,
  2026-11-16: handlers were listed as `info`; the sweeps, #472 and #473,
  made them `debug`)*. With the OTLP span filter at `info`, the helper spans are
  disabled callsites (one atomic load each) and cost nothing; raising
  `traces_filter` to `debug` or `trace` gives the full call tree.
- **`skip_all`, always,** with safe values through `fields(...)`.
- **MCP tools.** The `info` span of a tool call comes from
  `OkfServer::call_tool`; each tool method takes
  `#[tracing::instrument(level = "debug", skip_all, ...)]` below
  `#[tool]`. *(Corrected, M15 plan, 2026-11-15.)* The
  `RequestContext` argument carries the request's headers, the bearer
  credential among them. Functions the rmcp macros generate are not
  flagged.
- **Exemptions** are
  `#[cfg_attr(dylint_lib = "<lib>", allow(uninstrumented_fn, reason = "..."))]`
  on a function, `impl` block or module, with a reason from this list
  (anything else needs review):
  - trivial accessors and conversions (group them in one exempt `impl`);
  - `Display` and `Debug` impls and the helpers only they call (a span
    there re-enters the subscriber while it formats);
  - per-byte or per-element predicates in a hot loop (the `okf` parser);
  - one-line adapters that only call an instrumented function;
  - code that runs before the subscriber exists (`main`,
    `Telemetry::start`, the subscriber builder);
  - exporter, processor and subscriber trait impls in `src/telemetry/`
    (a span there feeds back into the exporter);
  - constant trait plumbing (`JsonSchema` impls returning fixed schemas).
- **Rollout.** The lint is wired in warn mode first, the sweep runs module
  by module, and the last step makes it deny in `just dylint` and the
  `gate.sh dylint` gate (the repository has no test CI; no CI job is
  added). *(Corrected, review of PR #459, 2026-11-15.)*

## Testing

- **Subscriber composition.** `build_subscriber` is a pure function of the
  config; tests check each layer's filter with `would_enable` and
  `callsite_enabled` (not `enabled!`, whose callsite cache is shared
  across subscribers): stderr at `warn` with OTLP at `debug` still exports
  `debug` spans; `rmcp` above `info` and the export stack's targets never
  reach an OTLP layer.
- **Spans and logs.** `InMemorySpanExporter` and `InMemoryLogExporter`
  with simple processors (the `opentelemetry_sdk/testing` feature, dev
  only), under one permanent global test subscriber installed once per
  test binary (avoids callsite-interest races and the dispatcher deadlock
  when a provider drops inside it). Tests wait on a condition, never on a
  sleep, for a span to close.
- **HTTP span.** A request through the real router: `otel.name`, the route
  template, the i64 status, 5xx as error and 4xx not, an unmatched route
  producing no span, a forged `traceparent` producing a link and a fresh
  trace id.
- **Metrics.** A real `/metrics` listener scraped over TCP: every
  observable family (build info, export failures, the gauges) present
  before the first request, and each event-driven family after its first
  event, with no fake seeding. *(Corrected, review of PR #459, 2026-11-15.)* No duplicate `# TYPE` line, seconds
  buckets, no `target_info` or `otel_scope_*` labels, and an unknown tool
  name counted as `other`.
- **Mock OTLP collector.** An in-process axum server on `/v1/traces`,
  `/v1/logs` and `/v1/metrics` that decodes protobuf: the configured
  headers arrive, the Sentry DSN target derives the right path and
  header, a redirect is not followed, a 500 counts in
  `okf.telemetry.export.failures`.
- **Shutdown.** A hung exporter: shutdown returns within the deadline and
  names the abandoned flush.
- **Privacy sentinel.** Requests through MCP, the REST API, the console
  sign-in and an archive import carry sentinel strings in the bearer
  header, cookie, query, path segment, body and document text; nothing
  exported (spans, attributes, events, logs, metrics) contains any of
  them.
- **Config.** Validation of every rule above, with property tests for the
  header and DSN parsers.

## Operations

- `docs/operations.md` gains a "Telemetry" section: the `[telemetry]`
  table, OTLP to an OTLP receiver, to Sentry and through a collector, the
  metrics listener, the metric reference, and how to tune stderr down and
  read the receiver instead.
- `examples/kubernetes/`: a Service for the metrics port and a
  ServiceMonitor that selects it, and the downward-API environment for
  `OTEL_RESOURCE_ATTRIBUTES`. The ingress example is unchanged.
- `examples/grafana/okf-mcp.json`: an example dashboard; a test checks it
  references only metrics that exist.

## Delivery

One epic, with sub-issues:

1. **Telemetry core**: `[telemetry]` config and validation,
   `build_subscriber`, stderr text/JSON, OTLP spans and logs, the Sentry
   DSN target, the counting exporter, shutdown.
2. **Metrics provider and `/metrics` listener**: `Metrics`, the
   Prometheus reader, the listener and its overlap check,
   `okf.build.info` and `okf.telemetry.export.failures`.
3. **HTTP and MCP**: the server span, inbound links, the HTTP and MCP
   metrics, `#[instrument]` on the tools.
4. **Publish, changesets, poll loop and caches**.
5. **Auth, OAuth, rate limits and mail**.
6. **Postgres pool, clock check, archives, outbound propagation**.
7. **Privacy sentinel test and mock collector**.
8. **Dylint wiring and policy doc**, in warn mode, with a `gate.sh dylint`
   gate. *(Corrected, review of PR #459, 2026-11-15.)*
9. **The `#[instrument]` sweep**, in parallel: (a) `store`; (b) `okf`;
   (c) `http`, `oauth`, `oidc`; (d) `mcp`, `auth`, `mail`, `cli` and the
   small modules; (e) `investigator`, `bin`, `maintenance`.
10. **The lint to deny**.
11. **Operations docs, Kubernetes and Grafana examples**.

Order: 1, then 2, then 3 to 6 in parallel, then 7. 8 runs beside 1; the
sweep (9a-9e) follows 8 and runs in parallel; 10 follows all of 9. 11
follows 2 and 7.

Follow-ups, outside the epic: OTLP export for `okf-maintenance`; the
native Sentry SDK, if needed.

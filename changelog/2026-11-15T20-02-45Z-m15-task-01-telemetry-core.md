# M15 Task 1: telemetry core (issue #461)

## Added
- `src/telemetry/`: the `[telemetry]` config (`config.rs`, unknown keys refused, filters and endpoint validated), the subscriber with one filter per layer (`subscriber.rs`: stderr text or JSON, OTLP spans, OTLP logs), JSON lines with `trace_id` and `span_id` (`json.rs`), and the OTLP/HTTP targets, the Sentry DSN target, header parsing and counting exporters (`otlp.rs`).
- The OTLP cap is an allow-list: only `okf_mcp` targets reach OTLP, minus `okf_mcp::local_only` (`subscriber::LOCAL_ONLY`), which stays on stderr. `rmcp` is capped at `info` on stderr.
- `Telemetry::start` and `Telemetry::shutdown` (parallel flush on plain threads within 4 s, returning a `ShutdownReport`), `OTEL_SDK_DISABLED`, optional trace sampling.
- `telemetry::start_up` and `telemetry::install`: the start-up fallback subscriber covers config load and `Telemetry::start` only, and its guard is dropped before the global subscriber is set (`tests/telemetry_install.rs`).
- Dependencies: `opentelemetry*` 0.33.1, `tracing-opentelemetry` 0.34, tracing-subscriber `registry`.

## Changed
- `cli::run` installs the subscriber after reading the config; `serve` gets telemetry, every other subcommand stderr only. The serve body moved to `cli::serve`, so telemetry is flushed after a failed start too. `main` no longer installs a subscriber.
- `Config` gains `telemetry` and loses `Eq` (an `f64` ratio).
- M15 plan Task 1 corrected where the code differs (see its "Corrected (issue #461)" note).

## Review round 1 (PR #480)
- `LogMailer` logs to `okf_mcp::local_only`, so a reset link never reaches OTLP.
- The OTLP cap matches `okf_mcp` exactly or `okf_mcp::`, not `okf_mcp_other`.
- `OTEL_SDK_DISABLED` is checked before OTLP credentials are resolved.
- Context activation is explicit, and a test checks an exported log carries its span's trace and span ids.

## Review round 2 (PR #480)
- The JSON line's `trace_id`/`span_id` come from the whole subscriber (the captured `Dispatch`'s current span and the `Registry`'s ancestors), not the fmt layer's filtered context, so a warning inside an exported debug span carries its ids when stderr is at `warn` and traces at `debug`. If the span is not exported, the nearest exported ancestor's ids are used.

## Review round 3 (PR #480)
- `headers_env = "none"` (the documented no-auth setting) now resolves to no headers without reading the environment; it used to fail start-up asking for a variable named `none`. The Sentry DSN path needs a DSN variable and has no such sentinel.

## Notes
- Span-layer error-to-exception options are off so error text never becomes an attribute or status description.
- No metrics, request spans or privacy audit yet: Tasks 1b, 2 and 3.

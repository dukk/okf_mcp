# A per-signal switch for OTLP export (issue #524)

## Fixed
- With `[telemetry.otlp] sentry_dsn_env`, every metrics push failed with a 404 (WARN and ERROR once per `export_interval`, and `okf_telemetry_export_failures_total{signal="metrics",reason="rejected"}` climbing): Sentry's OTLP intake takes traces and logs only. A Sentry DSN now exports traces and logs by default and no metrics.

## Added
- `[telemetry.otlp] signals`: any non-empty subset of `"traces"`, `"logs"` and `"metrics"`. Default: all three with `endpoint` (no change for existing files); `["traces", "logs"]` with `sentry_dsn_env`.
- Refused at start-up: an empty list, a duplicate, an unknown name (serde rejects it), and `"metrics"` with `sentry_dsn_env` (the message points to `[telemetry.prometheus]`).
- A signal that is off builds no exporter and no span layer, log layer or periodic reader. Its `traces_filter`, `logs_filter`, `sample_ratio` and `export_interval` still parse.
- The start-up line `OTLP export is on` carries `signals=...`, the signals whose exporter is running.
- Tests: config parsing, defaults and refusals (`telemetry::config`); the start-up line, through `install_with_writer` (`tests/telemetry_install.rs`); no exporter for a disabled signal, all twelve failure series at 0, and a stub receiver that sees only the Sentry traces and logs paths (`telemetry::tests`).

## Notes
- `okf_telemetry_export_failures_total` keeps all twelve series from the first scrape; a disabled signal's stay at 0.
- `docs/operations.md` has the `signals` key and the Sentry note.

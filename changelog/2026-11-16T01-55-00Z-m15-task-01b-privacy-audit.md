# M15 Task 1b: privacy audit of existing tracing events (issue #462)

The OTLP log bridge exports every `okf_mcp` event that passes its filter, so
the events the crate already emitted had to be checked against the spec's
"never exported" list. A grep-based scan of all of `src/` (including
`src/telemetry/**`, `src/bin/**`, and files with an early `#[cfg(test)]` module)
found 76 `split_event!` calls and 47 direct event calls in production code; each
was classified. The first pass missed `settle` (`src/http/api/mod.rs`, a
scanner gap) and three events in `src/telemetry/mod.rs` (found in review).

## What changed

- New macro `split_event!` (`src/telemetry/subscriber.rs`) writes an exported
  event plus a `okf_mcp::local_only` twin ending in `(detail)`.
- Error text (`%error` of `OkfError`, `sqlx`, IO, WebAuthn, OIDC and mail
  provider errors) moved to the twin. The exported event keeps `kind` where
  the error has one (`OkfError`), otherwise only its message.
- Caller text moved to the twin: OAuth client ids and refusal reasons
  (exported: `reason = error.code()`), settings values, IP addresses of
  sign-in attempts, file names and paths from workspace and snapshot scans,
  consent client ids.
- Principal text moved to the twin: `actor`, `user = %user.name` and `key_id`.
  A console session's key id is `session:<name>`, so key ids carry names.
  Exported events keep user ids, job ids and counts.
- Left alone: events under a target outside `okf_mcp`
  (`okf_maintenance::rejection`, the `okf-maintenance` binary), and values
  from operator config (listen address, NTP server, default catalog).
- `src/telemetry/mod.rs`: the shutdown-thread error is split (`warn_thread_not_started`)
  and the two `OTLP ... are off: {error}` startup warnings now export only the
  signal name, with the error on stderr (`warn_provider_off`). The shutdown
  thread case cannot be forced in a test, so both helpers are tested directly.
- `LogMailer` was already moved by Task 1 (issue #461).

## Why

`skip_all` protects only the arguments of functions instrumented from now on.
Without this audit the release that turns OTLP on would export client ids,
error messages that can name paths, and user names.

## Tests

Per-site tests with the in-memory exporters: a refused OAuth client id, an
internal request error, an internal tool error, the telemetry startup and
shutdown helpers, and the macro itself. The revert check was run for the
refused client, the tool error and the telemetry helper; each test failed.
The request-error and macro tests were not revert-checked.

## Plan and docs

Plan Task 1b carries a dated correction note. `docs/contributing/policy-lints.md`
documents `split_event!` and the principal-text rule.

# M15 Task 1b: privacy audit of existing tracing events (issue #462)

The OTLP log bridge exports every `okf_mcp` event that passes its filter, so
the events the crate already emitted had to be checked against the spec's
"never exported" list. About 110 non-test sites were classified.

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
- `LogMailer` was already moved by Task 1 (issue #461).

## Why

`skip_all` protects only the arguments of functions instrumented from now on.
Without this audit the release that turns OTLP on would export client ids,
error messages that can name paths, and user names.

## Tests

Per-site tests with the in-memory exporters: a refused OAuth client id, an
internal request error, an internal tool error, and the macro itself. Each
fails when its site is put back (checked for the first and the tool error).

## Plan and docs

Plan Task 1b carries a dated correction note. `docs/contributing/policy-lints.md`
documents `split_event!` and the principal-text rule.

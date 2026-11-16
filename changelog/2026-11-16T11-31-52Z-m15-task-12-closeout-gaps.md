# M15 Task 12: observability close-out gaps (issue #497)

## Added
- `db.system.name = "postgresql"` and `db.operation.name = "<module>.<function>"` on the `#[instrument]` span of every function in `src/store/**` that takes a pool, connection or transaction (260 spans; `publish_locked` runs inside the tagged `okf.publish` span and is not tagged itself). No SQL text or bind value is recorded.
- `method` and `outcome` on the auth spans: `signin::audit`, `signin::start_in` (through `signin::record_attempt`), `okf.oauth.token` (`method = "oauth_token"`) and OAuth authorize's `check_code` and `finish` (`method = "oauth_authorize"`, with a `Decision` drop guard). The outcome spellings are those of `okf_auth_attempts_total`.
- `secrecy` 0.10.3: the OTLP header values and the Sentry DSN are `SecretString`s until `OtlpTarget::exposed_headers` hands them to an exporter.
- A warning when `telemetry.otlp.endpoint` ends in `/v1/traces`, `/v1/metrics`, `/v1/logs` or `/v1`, with userinfo, query and fragment removed (`config::endpoint_with_signal_path`, `warn_if_endpoint_has_signal_path`).
- `telemetry::sampler_for`, so the configured sampler can be built under test.
- Tests: a store span's attributes, the sign-in, token and authorize span fields (with secrets absent), header secrecy, the `/v1` warning, `sample_ratio` at 0.0 and 1.0, and `http.request.method = _OTHER`.

## Changed
- `OtlpTarget::headers()` returns `HashMap<String, SecretString>`; `OtlpTarget` is no longer `PartialEq`/`Eq`.
- `oauth::token::token` wraps `issue_token`, which records the grant on the wrapper's span.
- The design and `docs/contributing/policy-lints.md` say handlers use `debug`, and the design's Status says delivered; its "nothing new is stored" names migration 0018. The plan has dated notes for each change.

## Notes
- The warning is emitted from `Telemetry::start`, not `Config::validate`: validation runs before any subscriber exists, so a warning there would be lost.
- Auth outcome is recorded only where the audit counts an attempt, so an error path out of `start_in` has no outcome.
- A refusal forwarded to an OAuth client's redirect is a page, not a sign-in, so the authorize handlers carry no outcome of their own.
- `tests/telemetry_privacy.rs` needed no new sentinel: the new attributes are constants or closed enums.

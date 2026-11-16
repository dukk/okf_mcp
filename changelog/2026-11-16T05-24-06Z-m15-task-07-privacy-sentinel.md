# M15 Task 7: mock OTLP collector and privacy sentinel test

## Added
- `tests/support/otlp_collector.rs`: an in-process OTLP/HTTP collector that decodes traces, logs and metrics (protobuf), records headers and raw bodies, serves any `/v1/<signal>` path (the Sentry prefix included), never follows redirects, and can answer 500 or 307.
- `tests/telemetry_privacy.rs`: collector tests (`configured_headers_arrive`, `a_sentry_dsn_derives_path_and_header`, `a_redirect_is_not_followed`, `a_500_counts_as_a_failure`) and the end-to-end sentinel test `nothing_secret_is_exported_or_scraped`. It plants synthetic secrets in every input channel, drives sign-in, a console session, MCP, publish, archives, mail, OAuth refusals and forced 5xx errors, flushes through `Telemetry::shutdown`, asserts positively that spans, logs and metrics arrived, and only then scans every exported byte and `/metrics` for the secrets.
- `TestServer::start_serving` (optionally runs the mail worker) and `TestServer::stop`.
- Dev-dependencies `opentelemetry-proto` and `prost` (already in the tree).

## Changed
- Plan Task 7 has a dated correction note describing what shipped.

## Notes
- Revert checks: recording `url.full` in `request_span` fails the test (forbidden attribute key); moving `client_id` into the exported half of the `refused an OAuth client` event fails it on the value scan.
- Run against the tree with the #472 sweep merged: no leak.

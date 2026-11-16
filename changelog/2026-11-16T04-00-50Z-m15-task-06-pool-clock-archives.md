# M15 Task 6: pool, clock check, archives and outbound trace context

## Added
- `db.client.connection.count{state="idle"|"used"}`, an observable gauge over a `Weak<App>` reading the pool's own counters (no Postgres query on a scrape). `db.client.connection.wait_time` stays dropped (plan deviations).
- `okf.clock.checks{outcome}` and `okf.clock.offset` (seconds, `f64`), recorded by `check_clock_until`; each check runs in a root `okf.clock.check` span (`parent: None`).
- Spans `okf.archive.export` and `okf.archive.import` with `okf.catalog` and `okf.archive.size` (`i64`), plus `okf.archive.duration{direction,outcome}` and `okf.archive.size{catalog,direction}` (rendered `okf_archive_size_bytes_total`). The catalog is the loaded `CatalogState`'s own name, recorded only after admission; an unknown name records nothing.
- `telemetry::propagation::inject`: a `traceparent` for the span the request is made under (`tracing::Span::current().context()`), on the OIDC adapter, the CIMD fetcher and the Postmark mailer.
- Tests: `pool_gauges`, `each_clock_check_is_its_own_trace`, `archive_export_and_import_are_measured`, `an_unknown_catalog_leaves_no_span_attribute_and_no_series`, `outbound_requests_carry_traceparent` (x3), `inject_*` (info and trace filters, and an unrelated active OpenTelemetry context).

## Changed
- `Metrics` keeps its `Meter` so gauges can be registered once the `App` exists.
- The plan's Task 6 has a dated correction note.

## Notes
- `inject` is not instrumented on purpose: a span of its own would become the current span and the header would name the helper. It is exempted from the lint with that reason.
- An admitted catalog that is not loaded yet records no archive metric (not even a duration), the same as an unknown name.
- Revert-checked: replacing `Span::current().context()` with `Context::current()`, removing `parent: None`, and recording the raw path name each made its test fail.

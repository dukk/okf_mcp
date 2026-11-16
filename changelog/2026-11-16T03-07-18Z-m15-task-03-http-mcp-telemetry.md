# M15 Task 3: HTTP and MCP spans and metrics

## Added
- The root request span (`parent: None`, name `<METHOD> <route>`, semconv method and route, `http.response.status_code` as `i64`, error status on 5xx only). A request no route matched gets no span. An inbound `traceparent` becomes a span link (`telemetry::propagation::link_from`, ids and flags only), never the parent.
- `RequestSpan` and the `request_telemetry` middleware: the auth middlewares record `enduser.id` (the user's id) and `okf.key.id` on the span handle, not on `Span::current()`.
- `OkfServer::call_tool`, the single choke point: an `mcp <tool>` span with `okf.tool`, `okf.catalog` (a loaded catalog, else `none`), `okf.outcome`, `okf.error.kind` (recorded by `tool_error` through the `TOOL_SPAN` task-local), `enduser.id`, `okf.key.id`.
- Metrics: `http.server.request.duration`, `http.server.active_requests`, `okf.mcp.tool.duration`, `okf.mcp.sessions.active`.
- Test helpers `Observer` (a subscriber scope across `.await`) and `McpProbe` (an MCP client over a loopback socket); tests at the `info` and `trace` span filters.

## Changed
- `okf.key.id` is `session` for a console session (its `key_id` is `session:<user name>`, and a name never goes in a span attribute). `Principal.key_id` is unchanged; `auth::span_key_id` maps it where the attribute is recorded, through `auth::record_principal_ids`, used by both spans.
- The request log line's span fields are now `http.request.method` and `http.route`; there is no `<unmatched>` span.
- The plan (Task 3 Corrected note) and the spec's HTTP and MCP rows and Privacy bullet state the rules above.

## Notes
- Metric labels are closed sets: semconv method, route template, status code, tool name (or `other`), loaded catalog name (or `none`), outcome.
- A console session is not an MCP credential, so its test covers the HTTP span only.

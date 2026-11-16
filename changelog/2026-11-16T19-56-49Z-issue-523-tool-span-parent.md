# The `mcp <tool>` span is a child of the `POST /mcp` request span (issue #523)

## Fixed
- `call_tool` opened its `mcp <tool>` span with no parent: `StreamableHttpService` in stateful mode runs the handler on the session's worker task, so `Span::current()` was empty and the span was exported as a root of its own trace. The span now takes the request span the HTTP layer already stores in the request's extensions (`http::RequestSpan`, read from the `Parts` in `context.extensions`) as its parent, so one trace shows request, tool and, with a raised `traces_filter`, the tool's own spans.

## Added
- `mcp::tool_span_parent`: the parent id, from the request span, else the current span, else none (a root, as before).
- Tests in `mcp::telemetry_tests`: a real session over loopback asserts the exported tool span's parent is `POST /mcp` in the same trace at both filters, with children under the tool span at `trace` (it failed with the parent removed); a unit test of the fallback order (request span, current span, root).

## Notes
- No new attributes; the privacy sentinel tests still pass.
- Holding the `RequestSpan` clone in the request's extensions keeps the request span open until the tool call ends, so its exported duration can now cover the call. The `http.server.request.duration` metric is measured by `request_telemetry` and is unchanged.
- `initialize`, `tools/list` and notifications never reach `call_tool` and stay single-span requests.

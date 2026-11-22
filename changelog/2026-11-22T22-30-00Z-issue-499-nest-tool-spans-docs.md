# Docs: the `mcp <tool>` span is nested under the `POST /mcp` request span (issue #499)

## Changed
- `docs/operations.md`, "Span reference": the paragraph that said a tool call is a trace of its own and to find a call by its tool span now says the tool span is a child of the request span in one trace, that the request span stays a root with the inbound `traceparent` as a link only, and that `initialize`, `tools/list` and notifications stay single-span requests.
- The observability design's MCP row records the parent, with a dated correction note.

## Notes
- No code change. Issue #523 (commit 695979b) already parents the span on `http::RequestSpan` in the request extensions; this issue's remaining work was the stale documentation.
- Confirmed in rmcp 3.8.0 (`streamable_http_server/tower.rs`) that the request's `http::request::Parts` is inserted into the request extensions that reach `call_tool` as `RequestContext::extensions`. The test `mcp::telemetry_tests::the_tool_span_is_a_child_of_the_request_span` proves the nesting over a real session at both trace filters.

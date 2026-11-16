# Issue 505: drop the SSE priming event from legacy-session replies

Google Gemini's MCP connector failed the handshake. For protocol versions
before 2026-07-28, rmcp streams every POST reply and, with `sse_retry` set
(its default of 3 s), prepends an event with empty `data`. `mcp_service`
now sets `sse_retry` to `None` twice: on the HTTP config (the `initialize`
reply) and on the session config (later POSTs such as `tools/list`; rmcp
reads a separate field there). The first event of each reply is the
JSON-RPC result. Nothing is lost: `LocalSessionManager` keeps no event store to
resume POST streams from. 2026-07-28 clients are served statelessly and
are unaffected. Tests in `tests/server.rs`:
`a_legacy_initialize_reply_has_no_empty_priming_event` and
`a_legacy_session_request_reply_has_no_empty_priming_event`.

If Gemini still fails after deploying, the issue's fallback is
`legacy_session_mode = false` plus `json_response = true`.

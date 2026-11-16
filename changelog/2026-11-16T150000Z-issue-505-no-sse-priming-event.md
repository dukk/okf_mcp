# Issue 505: drop the SSE priming event from legacy-session replies

Google Gemini's MCP connector failed the handshake. For protocol versions
before 2026-07-28, rmcp streams every POST reply and, with `sse_retry` set
(its default of 3 s), prepends an event with empty `data`. `mcp_service`
now sets `sse_retry` to `None`, so the reply's first event is the JSON-RPC
result. Nothing is lost: `LocalSessionManager` keeps no event store to
resume POST streams from. 2026-07-28 clients are served statelessly and
are unaffected. Test: `a_legacy_initialize_reply_has_no_empty_priming_event`
in `tests/server.rs`.

If Gemini still fails after deploying, the issue's fallback is
`legacy_session_mode = false` plus `json_response = true`.

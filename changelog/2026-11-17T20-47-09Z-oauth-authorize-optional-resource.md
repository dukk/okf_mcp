# OAuth: `resource` is optional at authorize

## Problem

Connecting Crush (v0.101.0) to the server failed in the browser with
`invalid_target`: "resource must name this server's MCP endpoint". Crush
deletes the RFC 8707 `resource` parameter from the authorization URL it opens
(`internal/oauth/mcp/handler.go`, `stripResourceParam`), because some
authorization servers reject it there, and sends it only to the token
endpoint. `GET /oauth/authorize` required it.

## Change

- `src/oauth/authorize.rs`: a missing `resource` at authorize now means this
  server's one resource, `<public_url>/mcp`. A `resource` naming anything else
  is still `invalid_target`. The code is stored for `/mcp` as before, and the
  token endpoint still requires `resource` and checks it against the code, so
  every token stays bound to `/mcp`.
- Tests: `an_authorize_request_without_resource_is_for_the_mcp_endpoint` runs
  the whole flow without `resource` at authorize and exchanges the code; the
  "missing resource is `invalid_target`" case is removed.
- Docs: `docs/operations.md` (Audience) and the OAuth design spec's RFC 8707
  row.

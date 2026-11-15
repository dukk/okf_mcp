# Policy lints (dylint)

One project policy is enforced by a [dylint](https://github.com/trailofbits/dylint) lint
library from [hyp-rust-policy-lints](https://github.com/hypotheosis/hyp-rust-policy-lints),
pinned by tag in `[workspace.metadata.dylint]` in the root `Cargo.toml`. Only its
`hyp_tracing` library is loaded (`pattern = "lints/hyp_tracing"`); the `hyp_hegel` library
(property-test policy) is not.

| Library | Lint | Fires on |
|---|---|---|
| `hyp_tracing` | `uninstrumented_fn` | A non-test function without `#[tracing::instrument]` |
| `hyp_tracing` | `instrument_exemption_without_justification` | An `allow` or `expect` of the above with no `reason` |

`const fn`, closures and code compiled only for tests are out of scope for
`uninstrumented_fn`.

**Level.** The lint is wired at warn level while the `#[instrument]` sweep of milestone 15
runs (`just dylint` passes `-W uninstrumented_fn`). The last sweep step makes it deny.
Until then, `tools/agentic/gate.sh dylint` passes with warnings, and every task that adds
functions must add no new warning in its own code.

## Running

```bash
cargo install --locked cargo-dylint dylint-link   # once
tools/agentic/gate.sh dylint                      # shared lock, 45 m limit
cargo dylint list --all                           # must list uninstrumented_fn
```

The first run builds the lint library on its own pinned nightly, which takes a few minutes.
`just dylint` passes `--all-targets` so test targets are compiled and excluded properly. If
`cargo dylint list --all` does not list the lint, nothing was checked.

`[workspace.metadata.dylint]` is the only table dylint reads (`[package.metadata.dylint]` is
ignored: "No libraries were found"). That is why `Cargo.toml` has an otherwise empty
`[workspace]` table. Its `exclude = [".worktrees"]` keeps git worktrees nested in the
checkout, whose branches may predate the table, from failing with "current package believes
it's in a workspace when it's not".

Changing the lint level (`-W` to `-D`) does not invalidate Cargo's cache, because lint flags
are not part of the fingerprint: a cached run replays the old diagnostics. After changing the
level, `touch src/lib.rs src/main.rs src/bin/okf-maintenance.rs` before running the gate.

## Instrumenting a function

```rust
#[tracing::instrument(level = "trace", skip_all, fields(user_id = user.id()))]
fn load_profile(user: &User, name: &str) -> Result<Profile, OkfError> { ... }
```

- **Level.** Public entry points into a module use `debug`; private helpers use `trace`.
  MCP tools, HTTP handlers, publish and the background roots use `info`. With the OTLP span
  filter at `info`, the helper spans are disabled callsites (one atomic load each) and cost
  nothing. Raising `traces_filter` to `debug` or `trace` gives the full call tree.
- **`skip_all`, always.** `#[instrument]` records every argument with `Debug` unless told
  otherwise, and a request's arguments carry credentials, document text and caller input.
  Record values only through `fields(...)`.
- **Safe field values.** Catalog names that exist, counts, generations, `user_id`, `key_id`,
  enum discriminants (`as_str`, `kind()`), and sizes recorded as `i64`. Never a name, an
  email address, a path, a query, document or concept text, SQL, a URI, a header, a token or
  an error message.
- **Validate before recording.** A span field or metric label taken from a path segment,
  query or body is recorded only after it has been validated against server state: a loaded
  catalog, an existing user id. Declare the field `tracing::field::Empty` and record it once
  the check has passed. A refused or unknown value never reaches a span or a metric series.
- **Integer fields are `i64`.** Other integer types export as strings.
- **Never `err` or `ret`.** `err` logs every `Err` at ERROR, which is wrong for validation
  failures, and records the error's text. `ret` records the return value.
- **MCP tools (`rmcp` macros).** The functions that `#[tool_router]`, `#[tool]` and
  `#[tool_handler]` generate are not flagged and need no exemption. The `info` span of a
  tool call comes from `OkfServer::call_tool`; each tool method takes
  `#[tracing::instrument(level = "debug", skip_all, ...)]` below `#[tool]`. The
  `RequestContext` argument carries the request's headers, the bearer credential among them,
  so `skip_all` is not optional.
- **`async fn`.** `#[tracing::instrument]` instruments the returned future, so no manual
  `.instrument(...)` is needed.

## Events

`skip_all` protects arguments only. An event (`info!`, `warn!`, `error!`, ...) has three
kinds of content that must not be exported:

- **Caller text:** a client id, a redirect URI, an email, a name, a path, a query value, a
  header.
- **Error text:** `%error` of an `OkfError`, `sqlx::Error`, `reqwest::Error` or a provider
  error. Its message can name a path or echo input.
- **Content:** a document, a mail body, a link.

Send all three to the target `okf_mcp::local_only` (the `LOCAL_ONLY` constant in
`src/telemetry/`), which both OTLP layers turn off, so they reach stderr only. The exported
event beside it keeps a closed value (`kind`, a count, a status):

```rust
tracing::error!(kind = error.kind(), "request failed");
tracing::error!(target: LOCAL_ONLY, error = %error, "request failed (detail)");
```

A reviewer checks every new event against this list.

## Exempting

Wrap the `allow` in `cfg_attr(dylint_lib = "hyp_tracing", ...)` so ordinary builds do not
warn about an unknown lint (`[lints.rust.unexpected_cfgs]` in `Cargo.toml` declares the cfg),
and always give a `reason`. It can sit on a function, an `impl` block or a module.

```rust
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(uninstrumented_fn, reason = "trivial field accessor: returns a field")
)]
```

The lint only checks that a reason exists. Reviewers check that it is true. These are the
accepted reasons. Anything else needs a review discussion.

- **Trivial accessors and conversions.** The span would cost more than the body and record
  nothing new. Group them in one `impl` block and exempt the block.
- **`Display` and `Debug` impls, and the helpers only they call.** They run inside the
  subscriber while it formats an event, and opening a span there re-enters it.
- **Per-byte or per-element predicates in a hot loop** (the `okf` parser).
- **One-line adapters** that only delegate to an instrumented function.
- **Code that runs before the subscriber exists:** `main`, `Telemetry::start` and the
  subscriber builder. A span there is recorded nowhere.
- **Exporter, processor and subscriber trait impls in `src/telemetry/`.** A span there feeds
  back into the exporter.
- **Constant trait plumbing,** such as `JsonSchema` impls that return fixed schemas.
- **Span makers and code that captures the enclosing span:** `request_span`, the
  `on_response` hook and `capture_request_span`. Their own span would become the request's
  parent, or be captured in place of it.

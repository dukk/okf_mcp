# M15 Task 9c: instrument http, oauth, oidc

## Added
- `#[tracing::instrument(skip_all)]` on 296 functions in `src/http/**`,
  `src/oauth/**` and `src/oidc.rs`: `level = "debug"` on handlers, async
  functions and `pub` entry points (230), `level = "trace"` on private
  synchronous helpers (66). No span records a field, so no request, body,
  header, token, password, email, client id, code, URL or user name can reach
  a span. The request span, and the `info` spans the handlers already had,
  are unchanged.
- 139 `uninstrumented_fn` exemptions, each with a reason from the accepted
  list in `docs/contributing/policy-lints.md` unless noted:
  - 77 trivial accessors, conversions and one-line helpers;
  - 30 router builders (`routes`, `api_routes`, `router`, `mcp_service`): they
    run once at startup, and `an_unmatched_path_makes_no_span` requires that
    nothing at all is exported for an unmatched request, which a span from a
    router built under the observer would break;
  - 17 `Display`/`Debug` impls;
  - 8 trivial constructors;
  - 2 per-element predicates (`oauth::pkce`);
  - 2 functions that call `telemetry::propagation::inject`
    (`oidc::send`, `oauth::clients::CimdFetcher::fetch`): the injected
    context must be that of the span the request is made under;
  - 1 one-line delegation (`BearerVerifier::verify` for the OAuth cache).
  - Not on the accepted list, so named in the PR: the router builders
    above, `GuardedResolver::resolve` (it returns a boxed future, so a span
    would cover only building it) and `http::serve` (the server's own
    lifetime, so its span would stay open until shutdown).

## Changed
- The dylint warning count in these paths goes from 435 to 0. The total goes
  from 1295 to 860; other paths keep theirs until their sweep PRs merge.
- No behaviour change. The span makers and request-span code
  (`request_span`, `record_response`, `request_telemetry`, `link_from`,
  `inject`) were already exempt or untouched, and the archive helpers that take
  the handler span as a parameter are as they were.

## Notes
- The `Corrected (issue #471)` and `(issue #474)` notes in the plan's Tasks
  9a-9e section needed no change: this group has no parallel pass and no
  `schema_with` field. A new `Corrected (issue #472)` note records the two
  exemptions beyond the accepted list.
- The sentinel test (`telemetry_privacy`) is not on `main` yet; Task 10 runs it
  over the swept tree.

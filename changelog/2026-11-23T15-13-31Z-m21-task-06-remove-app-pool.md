# M21 Task 6: every remaining caller picks a side; `App::pool()` goes (2026-11-23T15:13:31Z)

Issue #964, Task 6 of `docs/superpowers/plans/2026-11-23-okf-mcp-catalog-stores.md`.

## What changed

- `App::pool()` and the crate-wide `#![allow(deprecated)]` in `src/lib.rs` are removed.
- About 710 call sites (found by the compiler once the method was gone) in the HTTP API,
  sign-in, OAuth, OIDC, passkeys, mail, feedback, housekeeping, telemetry, invalidate and the
  tests now call `app.accounts().executor()`. The store-layer functions they call take an
  executor (`&PgPool`, decision P2), so the executor accessor is the right hand-off; none of
  them gained a new parameter type.
- Two callers are catalog data and use `app.main_store().executor()`: housekeeping's
  `changesets::prune_empty` and the `changesets::open_total` count in `cluster_stats`. Both
  read only `changesets` and `changeset_entries`.
- `catalogs::row_by_id` in `ToolContext::resolve_changeset` (left by Task 3) reads the
  registry, an account table, so it uses `accounts()`.
- No caller holds a transaction on one handle while taking a connection on the other: the
  moved callers use one handle per function (`cluster_stats` takes its counts one after the
  other, with no transaction open).
- The plan's Task 6 has a dated correction note.

## Crossings

None new. The inventory of "crosses in release 1" in the Task 2 to 5 changelogs is unchanged;
the two `main_store()` callers touch no account table.

## Release 1 is deployable here

No schema change and no behaviour change: both handles wrap the same pool in release 1.

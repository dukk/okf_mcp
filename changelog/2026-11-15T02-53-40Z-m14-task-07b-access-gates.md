# M14 Task 7b: access gates, exports and admin reads

Issue #401, part of epic #390. Plan: `docs/superpowers/plans/2026-11-13-okf-mcp-m14-operator-api.md`, Task 7b.

## Added

- `AccessGates` in `src/auth.rs`: team memberships per user, admin users, and the audit ids an access is recorded with (`audit_people` by user, `audit_credentials` by key or grant, and each catalog's `CatalogScope`: its `catalogs.id` and owner `audit_teams` id). The key cache loads it once per reload and every principal shares it (`Principal.gates`, `Keys::gates()`), as the suspension map is shared.
- `src/store/gates.rs`: `load`, which first creates any missing audit row (`ensure_audit_rows`: one statement per row, outside a transaction, so none can deadlock with a user deletion), then reads the maps.
- `Admission` (`Customer`, `Untracked`, `AdminRead`) and `Principal::admission`: an admin credential reading a catalog outside its user's teams is an admin read; a key with no user always counts; writes and non-admin reads are not tracked.
- `Principal::access_record` and `store::audit::AccessRecord`: every identity and scope is fixed at admission from the key cache.
- `store::audit::record_admin_read`: one `catalog.admin_read` entry per actor audit id, catalog identity, owning team and clock hour, with an exact request count in `audit_access_counts`. It uses only the ids it is given, so it creates no person or credential row.
- `ToolContext::admit`, which notes each catalog and kind at most once per call. `App::note_access` writes on a tracked `JoinSet` off the request path, `App::settle_access` waits for those writes, and `http::serve` waits for them (up to 30 seconds) at shutdown.
- `catalog.exported` for every export (CLI, REST and console), written in its own transaction before compressing. An export whose entry fails also fails.
- Tests:
  - `src/http/api/access_tests.rs`: an operator's reads are bucketed and a member's are not; a write delayed past the user's deletion stores nothing identifying; a read before a transfer stays with the old owner; a deleted or replaced catalog keeps its identity; every export is recorded.
  - Audit buckets and exact counts.
  - Admission.
  - The cache loading the gates.
  - The CLI export.
  - A changeset call on a multi-catalog connection recording one admin read.
  - `ensure_audit_rows` for a pre-audit key.
- `mcp::testing::{staff_root, settle_access}`. `settle_access` has a timeout, so no test waits unbounded.

## Changed

- Signatures:
  - `KeyCache::from_records(records, ceilings, suspended, gates, now)`;
  - `Principal::for_session(user, ceiling, suspended, gates)`;
  - `store::export::export(pool, catalog, out, audit, now)`.
- `ToolContext` selection:
  - `select` and `one` admit what they select;
  - `resolve_call` and `target` resolve the same way and admit nothing.
  - `check_connection`, `list_catalogs`, `open_changeset` and `list_changesets` use the non-admitting forms, so only the catalog actually read is recorded (Decision 23).
  - `changeset` admits only an open changeset's own catalog.
- `docs/operations.md`: `catalog.exported` and `catalog.admin_read` in the audit table, and how admin reads are counted.
- Plan: dated `Corrected (issue #401)` notes in Task 7b and on the 9b points that build on it.

## Notes

- `gates::load` runs after the cache's other reads, not beside them. That way every listed credential has an audit id. Otherwise a key read in the same reload could fall back to the label bucket, which every API key shares.
- The revision route needs no admit call of its own: it reads through `history` and `ctx.one`, which admit, and the per-call set counts it once.
- Revert checks:
  - storing the credential's own label (and keying the bucket by it) fails the deletion test;
  - looking the catalog up by name when the write runs fails the transfer and replacement tests;
  - admitting in `check_connection`, or in `list_changesets` with a catalog, fails the multi-catalog test;
  - dropping the per-call set fails the bucketing test (3 counts for 2 requests);
  - dropping the export record fails both export tests.

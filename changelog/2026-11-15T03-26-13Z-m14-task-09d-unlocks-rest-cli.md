# M14 Task 9d: unlocks over REST and the CLI

Issue #408, part of epic #390. Implements Task 9d of
`docs/superpowers/plans/2026-11-13-okf-mcp-m14-operator-api.md`.

## Added

- `POST /api/v1/unlocks` (body `{catalog, category, reason, hours?, user?}`,
  `deny_unknown_fields`; `hours` defaults to `unlock_max_hours`): an admin
  at `admin` on the catalog unlocks it for its own user, or, reaching every
  catalog, for another admin user. 201 with the full record. A key with no
  user is 403 ("give it a user"); a user who is not an admin is 409; hours
  outside 1 to the limit or a bad reason is 422 `LimitExceeded`.
- `GET /api/v1/unlocks?catalog=&team=&live=true`: admins read full records
  (the reason and the unlocked staff member), a scoped admin only those of
  the catalogs it administers. A team owner's console session naming the
  team reads that team's catalogs' unlocks redacted (`OwnerUnlockView`: the
  category, times, state and tallies, `by: "an operator"`, never the reason
  or who it was). Anyone else is 403.
- `DELETE /api/v1/unlocks/{id}`: any admin ends a live unlock (a scoped
  admin only one of a catalog it administers; any other id is 404). 409
  for one already ended or expired.
- `okf-mcp unlock create --catalog C --user U --category K --reason TEXT
  [--hours N]`, `unlock list [--catalog C] [--live]` and `unlock end ID`.
- `store::unlocks::UnlockState` and `UnlockRecord::state(now)` / `ends()`,
  shared by the views and `unlock list`.
- `tests/admin_parity.rs`: the three unlock commands in `COVERED`, and
  `unlock` in `SCANNED_GROUPS`.
- `docs/operations.md`: the three parity rows, an "Unlock before access"
  section with the commands, and an "Unlocks API" table (93 endpoints).

## Changed

- The plan: a dated `Corrected (issue #408)` note on Task 9d (scoped admins,
  the state helpers, the owner's scope, the test helper, the CLI and the
  docs section) and one on Task 9e (the two row shapes it draws).

## Notes

- The plan's `require_admin` alone would have let an admin key granted
  only another catalog unlock any catalog, which its user's console session
  (every catalog) could then open, and read every unlock's free-text reason,
  which the audit log shows only to every-catalog admins. Both routes now
  check the catalog.
- A credential with no user is refused before any target is chosen, so an
  unassigned every-catalog admin key cannot unlock for another user by
  naming one either (PR #437 review, round 1).
- Owners' redaction is structural: `OwnerUnlockView::of` takes the record
  by value and has no field for the reason or the person. The test checks
  the owner's whole unlock list, audit view and audit export contain neither
  `staff` nor the reason, and that the `target=staff` and
  `actor=staff-root` probes match nothing.

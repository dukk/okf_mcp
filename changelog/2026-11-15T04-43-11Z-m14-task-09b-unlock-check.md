# M14 Task 9b: the unlock check and the maintenance exemption

Issue #406, part of epic #390. With `require_unlock` on, operator staff open
a customer catalog's contents only under a live unlock; the maintenance
runner reads without one unless the catalog's owners turned maintenance off.

## Added

- `AccessGates` gains `entitled`, `require_unlock`, `unlocks` and
  `maintenance_off`; `gates::load(pool, now)` reads them in its one
  `REPEATABLE READ` snapshot (entitlements, the opt-outs, the live unlocks
  and the setting).
- `Admission::Maintenance` (the runner's read, not recorded) and
  `Admission::Unlocked { unlock_id }` (recorded as the unlock's tally).
- `Principal::check_open` (suspension, then the admission: used by the
  selection and the unnamed search and changeset lists) and
  `Principal::require_contents` (reach, level, suspension, then the
  admission: used by routes that read or write contents themselves).
- `store::unlocks::tally(pool, id, kind)`; `App::note_access` runs it for
  an unlocked access.
- Test helpers `mcp::testing::staff_key` and `mcp::testing::require_unlock`.
- `docs/operations.md`: who needs an unlock, what "contents" covers, the
  refusal, and the runner's exemption, under "Unlock before access".

## Changed

- `Principal::admission` refuses `MaintenanceOff` to the runner on an
  opted-out catalog and, with `require_unlock` on, `UnlockRequired` to
  everyone who is not a customer (an owner-team member, or an entitled
  non-admin) and has no live unlock of that catalog for their user.
- `selection::Sources` gains `now`; `refuse_suspended` and
  `without_suspended` became `refuse_closed` and `without_closed`, calling
  `check_open`.
- `ToolContext`'s changeset resolution, `open_changeset`, the revision
  route, the maintenance read tools and `maintenance_close`'s operator
  override use `require_contents`. Export checks the admission after
  `require_export`, import after `require(Admin)`, both before any work, and
  each tallies an unlocked request.
- `ToolContext::admit` answers `Busy` for any admission that would be
  recorded (an unlocked non-admin key included, not only an admin
  credential) when no gates know the incarnation read.
- `unlocks::live` and `settings::load` take `impl PgExecutor<'_>`.
- Export and import are bound to the catalog incarnation the admission was
  decided for (PR #442 review): `store::export::export` takes
  `ExportOptions { audit, now, expect_catalog_id }`, `ImportOptions` gains
  `expect_catalog_id`, and `catalogs::check_incarnation` refuses a catalog
  created again under the name with `Busy`, before anything is read or
  written. `open_changeset` refuses the same way. The CLI checks nothing.

## Notes

- Every existing test runs with `require_unlock` off, where nothing
  changes but the runner's reads, which are now `Maintenance` admissions
  (recorded as nothing, as before).
- Refusals come from the selection or `require_contents` before any
  `admit`, which ignores refusals, so a refused call queues nothing.
- The CLI works on the database directly and needs no unlock.
- The plan's Task 9b and 9c carry dated `Corrected (issue #406)` notes.

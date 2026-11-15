# M14 Task 9a: unlock storage, settings, error kinds and the owner email

Issue #405, part of epic #390. Implements Task 9a of
`docs/superpowers/plans/2026-11-13-okf-mcp-m14-operator-api.md`.

## Added

- Migration `0016_unlocks.sql`: the `unlocks` table (catalog, admin user and
  audit person, category, reason, start, expiry, end, read/write/export
  tallies) with its live, per-catalog and per-user indexes, and
  `catalogs.maintenance` (default `true`) for Task 9c.
- `store::unlocks`: `Category` (six categories with `as_str`, `parse`,
  `label`), `NewUnlock`, `UnlockPolicy`, `UnlockRecord`, `LiveUnlock`,
  `UnlockFilter`, and `create`, `end`, `find`, `find_in`, `list`, `live`.
  `create` records `catalog.unlocked` (category, reason, start, expiry) and
  queues the owner team's owners an email when `unlock_notify_owners` is on,
  in the same transaction; `end` records `catalog.unlock_ended`. Both bump
  `admin_generation`.
- Settings `require_unlock` (default `false`), `unlock_max_hours` (default
  4, 1 to 72) and `unlock_notify_owners` (default `true`). All three are
  recorded in full in `setting.changed` / `setting.cleared`.
- `OkfError::UnlockRequired { catalog, has_user }` (kind `UnlockRequired`)
  and `OkfError::MaintenanceOff { catalog }` (kind `Forbidden`, body adds
  `"maintenance": "off"`), both 403.
- Mail template `unlocked`: the catalog, the category and the end, never the
  staff member or the reason.
- `docs/operations.md`: the three Settings rows and two Troubleshooting rows.

## Changed

- `settings.rs`: the boolean settings share `parse_boolean` and one
  `as_bool` check (the `password_sign_in` refusal text is unchanged).

## Notes

- Lock order: `create` and `end` take every user row through
  `audit::lock_people` first (admins by id, as `users::set_admin` does), then
  the catalog or unlock row, then `server_state`. The plan's draft locked the
  unlocked admin with its own `FOR SHARE` and the actor later inside
  `record`; the race test
  `an_unlock_by_an_admin_and_a_concurrent_demotion_of_its_staff_both_finish`
  deadlocks (`Busy`) on that draft and passes on the shipped order.
- Nothing is awaited after a commit: the answer is read inside the
  transaction (`find_in`).
- The email's subject names the product (`An operator opened main in Acme
  Notes`), and its own "contact the operator" sentence is left out when a
  support contact is set, since `render` closes with that contact.
- The plan's Task 9a carries a dated `Corrected (issue #405)` note; Task 9e's
  refusal note now says 422 for an unlock over `unlock_max_hours`.
- Nothing calls the unlocks yet: Task 9b checks them at the content choke
  points, Task 9d adds REST and the CLI, Task 9e the console.

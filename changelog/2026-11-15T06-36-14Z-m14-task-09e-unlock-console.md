# M14 Task 9e: unlocking in the console, the Unlocks page and the maintenance switch

Issue #409, part of epic #390.

## Added

- `components/unlock.js`: the Unlock drawer (`#unlock-form`): category,
  reason (at most 500 characters) and hours (at most `unlock_max_hours`, read
  from `GET /settings`, 4 when it cannot be read). It posts `/unlocks` for
  the caller's own user. Without a catalog it offers a picker from
  `GET /catalogs`, or a text field when that read fails. The submit is a
  pending write keyed `unlock:<catalog>:create`, and a refusal stays in
  `#unlock-form-error`.
- `pages/unlocks.js`: the Unlocks page, in the Records nav group. An admin
  sees full rows (user, reason, until, tallies), can End a live unlock
  (`unlock:<id>:end`, with its refusal kept beside the row) and can "Unlock
  a catalog". A team owner picks an owned team and reads its unlocks
  redacted, by "an operator". Loads are numbered, and a failed reload keeps
  the last rows, marked `#unlocks-stale`.
- Browse shows `#browse-locked` for an `UnlockRequired` refusal, with an
  Unlock button for an admin with a user.
- The catalog drawer has a Maintenance section: `#catalog-maintenance`
  (`role=switch`) runs `PUT /catalogs/{c}/maintenance` as a pending write
  keyed `catalog:<name>:maintenance`. It keeps a refusal in
  `#catalog-maintenance-error` and redraws the drawer that shows that
  catalog now.
- A "Your teams" nav group with Catalogs, for account users who are not
  admins. For them the catalog drawer holds only the Maintenance section.
- `TestServer::owner_with_password`, and four journeys in
  `tests/ui_e2e/unlocks.rs`.

## Changed

- `core/api.js`: a 403 shows its own message for `Suspended`,
  `UnlockRequired` and `maintenance: "off"`, through one widened test. An
  `UnlockRequired` toast also carries an Unlock action.
- `pages/settings.js`: a `BOOLEAN_SETTINGS` set (`password_sign_in`,
  `require_unlock`, `unlock_notify_owners`) replaces the `password_sign_in`
  special case. The three unlock settings get descriptions.
- The plan's Task 9e has a dated `Corrected (issue #409)` note.

## Notes

- `whoami` has no admin-user flag. The console offers Unlock to an admin
  credential with a user, and the server refuses everyone else (409 or 403).
- `src/http/ui.rs` is unchanged: the manifest lists every embedded page.
- The Unlocks list is not paged (Task 9d), so it has no cursor to keep.

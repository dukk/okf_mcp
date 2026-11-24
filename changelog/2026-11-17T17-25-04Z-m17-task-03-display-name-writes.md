# M17 Task 3: store writes for display names

## Added
- `users::set_display_name`: sets a user's display name under the user's `FOR UPDATE` lock; a provisioner is refused an admin user (`Forbidden`), as `set_email` does. The personal team's column is left alone: its label follows the user.
- `teams::set_display_name` with `TeamDisplayNameChange { team, display_name, owner }`: refuses a personal team (`Refused`, 409) and, given `owner`, checks under the team's `FOR NO KEY UPDATE` lock that the user is an owner (`FOR SHARE` on the membership), else `Forbidden`.
- `catalogs::update_details` with `DetailsChange { catalog, display_name, description, owner }` and `Details`: sets the display name and/or the description in one `UPDATE`, returning both as they were; given `owner`, checks the owner team through `lock_owner` and refuses (`Forbidden`) a non-owner, an unknown or malformed catalog, and a catalog moved to another team while the change waited.
- `catalogs::lock_owner`, extracted from `set_maintenance`, which now calls it (its tests unchanged).
- Audit actions `user.display_name_changed`, `team.display_name_changed` and `catalog.display_name_changed`, each with details `{"length": n}` (characters, never the text). Each write bumps `admin_generation` only when something changed; the same text again writes and records nothing.
- Tests: set, record the length and bump, and the same text records nothing (users, teams, catalogs); a provisioner refused on an admin user; a personal team refused; owner yes and member no (teams and catalogs, with an unknown and a malformed catalog given the owner's same `Forbidden`); `update_details` records each changed field once and bumps once for two; an owner demoted while their team or catalog rename waits is refused (forced interleaving, under timeouts); an owner's catalog rename and their demotion at once both finish.

## Changed
- `catalogs::update_description` is replaced by `update_details`. Its callers (the CLI's `catalog update`, `PATCH /api/v1/catalogs/{catalog}` and the poll test in `src/app.rs`) pass `display_name: None, owner: None` and behave as before; the ported `update_description` tests go through a test helper that does the same.
- `docs/operations.md`: the audit table lists the three new actions.
- The spec's "recording the old and new value" is corrected to the length only (plan Decision 4).
- The plan's Task 3 is corrected (`> **Corrected (issue #600).**`): `update_details` re-checks the owner team under the catalog's row lock, and the team rename records before it bumps, as other team writers do.

## Notes
- Lock order (`tools/agentic/lessons.md`): users through `audit::lock_people` first; then for a team rename the team row and then the owner's membership; for a catalog change the owner's membership and then the catalog row, as `set_maintenance`; `server_state` last.
- A suspended team's owner editing a catalog label is refused at the HTTP layer in Task 5 (plan Decision 6), from the key cache's suspensions; the store does not check suspension, as `set_maintenance` does not.
- No route or CLI command sets a display name yet: Tasks 5 and 6 add them.

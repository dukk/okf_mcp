# M17 Task 10b: console admin pages, label-first users, teams and catalogs

Issue #608, part of epic #597.

## Changed

- Users: the first column is `named(user.label, user.name)` (the "you" and
  "admin" pills follow it); the separate Display name column is gone. The
  filter also matches the label. The drawer title is the label, with a
  "User ID" line, and a Display name field (`#user-display-name`,
  `PATCH /users/{name}`, a pending write) that is disabled for an admin who
  does not reach every catalog. The account toasts in
  `components/user-account.js` name the user by label.
- Teams: the first column is `named(team.label, team.name)`; members show
  `named(member.label, member.user)`; toasts use labels. A shared team's
  drawer has a Display name field (`#team-display-name`,
  `PATCH /teams/{name}`); a personal team's says "Named after its user" and
  has none. The drawer shows the team ID.
- Catalogs: the first column is `named(row.label, row.name)` and Owner is
  `owner_team_label`. `detailsEditor(row, ticket)` is the Display name plus
  Description form, sending only the changed fields in one
  `PATCH /catalogs/{name}`. The admin drawer uses it in place of the
  description form; `openMaintenance` became `openOwner`, which shows the
  editor (and the maintenance switch) to an owner of the catalog's team and
  only the switch to anyone else. The owner path reads no team list, which
  only an admin may read. The delete dialog still asks for the slug.
- Pickers (`components/picker.js`): `picker` and `assignControl` take an
  optional `labelOf`; options show the label and keep the slug as value.
  Used for the team owner, member add, entitlement catalog, unassigned
  assign and unlock catalog pickers.

## Tests

`users.rs`, `teams.rs` and `catalogs.rs` (ui_e2e): label cells and drawer
titles, each edit saving, a refused 81-character user name, no field for a
personal team, an owner session saving a catalog's display name while a
plain member sees no editor, and the slug-typed delete dialog.

## Added
- Team pickers say what each option is: a personal team reads "Alice Example (personal team)" (its user's label) and any other team "Acme (team)". Each option's value is still the bare team name, so the API calls are unchanged. This covers the catalog create form's Owner team, the Change owner control, and the unassigned view's catalog rows.
- For admins, the Catalogs list's Owner column adds a "personal team" or "team" pill beside the owner's label.
- `rowsOf(kind)`, `teamKindText(row)` and `teamChoices(rows)` in `components/picker.js`; `namesOf` is now built on `rowsOf`.

## Changed
- The unassigned view and the catalogs page read the team rows (not only names) for their pickers, and the unassigned view's "is now owned by" toast uses the kind label.

## Notes
- Display names (milestone 17) already label users in every user picker and a personal team in the Owner column as "Alice Example's team"; this change adds the kind to the pickers and the pill to the column.
- The pill is shown to admins only, because the list of teams needs admin. A non-admin still sees the team's label, which for a personal team reads "...'s team".
- Labels are built with `textContent` through `h()`, never HTML strings.

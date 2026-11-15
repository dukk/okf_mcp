# M14 Task 8c: the console's Audit page

Issue #404, part of epic #390. Plan:
`docs/superpowers/plans/2026-11-13-okf-mcp-m14-operator-api.md`, Task 8c.

## Added

- `src/http/ui/js/pages/audit.js`: the Audit page. An every-catalog admin
  reads the whole log, with a team-name filter; a person signed in with an
  account picks one of the teams they own (`GET /api/v1/me`'s
  `owned_teams`). Both filter by action (from the answer's `actions`),
  actor, target, since and until (`datetime-local`, sent as UTC RFC 3339).
  The table shows when, action, actor ("an operator" where the server says
  so), target (`user NAME` or `KIND NAME`; a user links to its drawer for
  an admin), teams and details (`N requests` for an admin read, "maintenance
  ran" for a maintenance run). Export downloads the same filters as
  `audit.jsonl`.
- `api.download(path, filename, opts)` in `core/api.js`: a GET answer saved
  as a file with the console's own credential (bearer key or session
  cookie), through `request(..., { raw: true })`.
- An `Audit` nav link in its own "Records" group, shown to every-catalog
  admins and to every account session (`shell.js`' `shows("audit")`).
- `#/users/<name>` opens that user's drawer on the Users page.
- `tests/ui_e2e/audit.rs`: `an_admin_filters_the_log_by_action_and_exports_it`,
  `an_owner_reads_its_team_and_sees_an_operator`,
  `a_late_table_load_never_draws_over_a_newer_filter` and
  `load_more_appends_only_to_the_newest_table_load`.

## Changed

- The plan's Task 8c gained a dated `Corrected (issue #404)` note, and Task
  9e's nav-link line a correction (no `ui.rs` change; the "Records" group).

## Notes

- Following `tools/agentic/lessons.md`' console rules: every table load is
  numbered and only the newest draws; "Load more" takes its cursor at once
  and appends only while its load is still the newest; a failed load or a
  refused export keeps the rows drawn, marked "may be out of date", with the
  server's reason on screen and every control usable.
- An empty page ends the list whatever its `next_after` (a full last page
  is followed by an empty one).
- The manifest needs no change: `ui.rs` lists every embedded page.
- The server enforces every redaction; the page only draws what it gets.

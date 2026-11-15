# Milestone 14: the operator API (epic #390)

An operator's own tooling can drive okf-mcp safely and leave a trail. The
product name people see is the operator's; a user can be found by email and
deleted in one call; a narrow provisioner key manages accounts, teams and
catalogs but reads nothing; every administrative change, catalog write,
export and operator read of other people's data is in a stored, append-only
audit log that admins read in full and team owners read for their team; and,
on a server that turns it on, operator staff must unlock a customer's
catalog, with a reason and a time limit, before opening it. CLI, REST and
console are at parity. Design:
`docs/superpowers/specs/2026-11-13-okf-operator-api-design.md`; plan:
`docs/superpowers/plans/2026-11-13-okf-mcp-m14-operator-api.md` (PR #375).

## Added

- **Task 1, audit storage and the write helper (#391, PR #412):** migration
  `0012_audit.sql` (`audit_entries`, `audit_people`, `audit_teams`,
  `audit_credentials`, `audit_access_counts`, the append-only trigger);
  `store::audit::{record, AuditActor, Entry, forget_person}` and the
  `Requester` (client IP and source) on every request.
- **Task 2, branding (#392, PR #413):** the `product_name` and
  `support_contact` settings in every email, the console and the server
  pages; `/ui/brand.json`.
- **Task 3, find a user by email (#393, PR #411):** `GET
  /api/v1/users?email=`, `okf-mcp user list --email`, and a filter box on
  the console's Users page.
- **Task 4, deleting a user (#394, PR #417; closes #379, meets #297's
  goal):** migration `0013_user_deletion.sql`; `user delete` takes the
  user's keys, grants, codes, sessions, sign-in methods, memberships and
  personal team, discards its open changesets, and keeps every catalog and
  its history; three refusals (an owned catalog, a last owner, the last
  admin user over REST).
- **Task 5, provisioner keys (#395, PR #422):** migration
  `0015_provisioner.sql`; `okf-mcp key create --provisioner`,
  `"provisioner": true` on `POST /api/v1/keys` and the console's switch; a
  16-route allow-list (`PROVISIONER_ROUTES`), no catalog reach, and admin
  users refused as targets under the user's lock.
- **Task 6a, recording user changes (#396, PR #419):** the eight `user.*`
  actions; migration `0014_audit_row_locks.sql` (resolvers that never lock
  an existing audit row).
- **Task 6b, recording team and catalog changes (#397, PR #420):** the
  `team.*` and `catalog.*` administrative actions and `audit::lock_people`,
  the one lock order every recorder takes.
- **Task 6c, recording credential changes (#398, PR #424):** the `key.*`
  and `oauth.*` actions, through `audit::lock_credential_people`.
- **Task 6d, settings and retention (#399, PR #416):** `setting.changed`
  and `setting.cleared`; `audit_retention_days` (400, 30 to 3650) and the
  daily prune with an hour's margin.
- **Task 7a, recording catalog writes (#400, PR #415):**
  `catalog.published`, `catalog.reverted`, `catalog.changeset_discarded`.
- **Task 7b, access gates, exports and admin reads (#401, PR #433):**
  `catalog.exported` for every export; hourly `catalog.admin_read` entries
  with exact counts, written in the background with ids fixed at admission;
  the key cache's `AccessGates`, loaded as one snapshot.
- **Task 8a, the audit log over REST (#402, PR #429):** `GET
  /api/v1/audit` and `GET /api/v1/audit/export`, with the owner redaction
  rules.
- **Task 8b, the audit CLI (#403, PR #436):** `okf-mcp audit list` and
  `okf-mcp audit export`.
- **Task 8c, the console's Audit page (#404, PR #438):** filters, Load
  more, Export, and an owner's team picker.
- **Task 9a, unlock storage (#405, PR #428):** migration
  `0016_unlocks.sql` (`unlocks`, `catalogs.maintenance`); the
  `require_unlock`, `unlock_max_hours` and `unlock_notify_owners` settings;
  the `UnlockRequired` and `MaintenanceOff` errors; the owners' email;
  `catalog.unlocked` and `catalog.unlock_ended`.
- **Task 9b, the unlock check (#406, PR #442):** operator access refused
  without a live unlock at every content path (MCP, REST browse and search,
  export, import, changesets, maintenance tools), the maintenance runner's
  exemption, the tallies, and catalog-incarnation binding.
- **Task 9c, the maintenance flag (#407, PR #444):** `okf-mcp catalog
  set-maintenance`, `PUT /api/v1/catalogs/{catalog}/maintenance` (an admin
  that reaches every catalog, or an owner of the owner team signed in to the
  console), `catalog.maintenance_changed`, and migration
  `0017_maintenance_audit.sql`'s `catalog.maintenance_run` trigger; the
  runner's `maintenance_off` skip.
- **Task 9d, unlocks over REST and the CLI (#408, PR #437):** `POST`,
  `GET` and `DELETE /api/v1/unlocks`; `okf-mcp unlock create|list|end`.
- **Task 9e, the console (#409, PR #445):** Unlock where a locked catalog
  is refused, the Unlocks page, the Maintenance switch, and the Records and
  "Your teams" nav groups.
- **Task 10, documentation (#410):** `docs/operations.md` (the lead on
  running okf-mcp for other people, the grouped audit action table with
  what is not recorded, people in entries, who reads it, the surfaces and a
  sample entry, the console's new groups, Upgrades and Troubleshooting),
  the README, and the spec's "What shipped differently".

## Changed

- Deleting a user deletes its credentials: `api_keys.user_id`,
  `sign_in_codes.user_id` and `team_members.user_id` are `ON DELETE
  CASCADE` (migration `0013`); `user delete` is no longer refused for a
  user who holds a key, a code or another team's membership.
- `okf-mcp key list` has a `provisioner yes|no` cell before `user U`, which
  moved from field 9 to field 10.
- Every email's subject names the product (`product_name`), and with a
  `support_contact` every email ends with "Questions? Contact ...".
- A write that changes nothing (the same value, flag, role, owner or
  notice again) no longer bumps the administrative generation, and records
  nothing.
- `GET /api/v1/catalogs` shows each catalog's `maintenance`; the console's
  Settings page shows the boolean settings as switches.
- The API has 94 endpoints (88 at the end of milestone 13): Audit (2),
  Unlocks (3), and Catalogs and import/export 10 (the maintenance route).
  Recounted from the routers for Task 10.

## Review findings worth knowing

Each became a rule in `tools/agentic/lessons.md`:

- **Every writer locks rows in one order, counting the locks `record`
  takes.** Recording at the end locked a user after the team, catalog or
  generation row, which deadlocked against `set_admin` and user deletion
  (PR #416, #397). `audit::lock_people` now takes every user an entry will
  resolve first, admins by id, then the rest.
- **Refreshing an audit reference must not wait on its row.** Upserting on
  every call locked existing `audit_people` rows, so two admins acting on
  each other deadlocked (PR #419); migration `0014` reads first and
  refreshes only under `SKIP LOCKED`.
- **An admission decided by catalog name binds to the incarnation it was
  decided for.** A catalog deleted and created again before the cache
  reloads could be read, exported or imported under the old one's unlock,
  or recorded against the wrong catalog (PR #433, PR #442).
- **A drawer opened from a table row reads settled state.** The catalog
  drawer's maintenance switch reopened showing the value before its write
  (PR #445).
- **A paged list's committed cursor survives a failed or overtaken read**
  (PR #438).

## Notes

- Gates at the close (Task 10, `tools/agentic/gate.sh`): `fmt` and `lint`
  passed; `test` passed on its first run with **2080 passed**, 0 failed and
  17 ignored across 20 test binaries and the doc-tests (library 1808,
  browser journeys 121, doc-tests 27). At the end of milestone 13 the same
  gate passed 1836 (the epic coordinator's record of M13's close), so this
  milestone added 244 tests.
- Migrations `0012` to `0017`; only `0013` changes behaviour (the
  cascades). `require_unlock` is off until an operator turns it on, so a
  self-hosted server behaves as before apart from the log being written.
  `docs/operations.md`, "Upgrades", has the line.
- **The provisioner's auditing records state changes, not every use.** The
  design said everything a provisioner does is audited; what shipped records
  every change it makes, but not its metadata reads, unchanged requests or
  refusals. #423 tracks it; it is not claimed complete. A provisioner's
  server-wide team and catalog operations may act on an admin user's
  personal team; named admin users as targets, owners or members are
  refused (the review of PR #422 found no bypass; see the maintainer's
  comment on #410).
- What differs from the design is in the spec's "What shipped differently":
  among others `lock_people` and the lock order; migration `0014`, which
  moved the later migrations up one; access recorded at the read (Decision
  23) and the gates loaded from one snapshot; catalog-incarnation binding;
  `FixedPerson` for an unlock's admin; scoped-admin unlock rules; the
  maintenance trigger; and the console's Records and "Your teams" groups.
- Follow-ups open at the close:
  - #418 a publish by a user's key can deadlock with deleting that user
    (lock order; retryable `Busy`);
  - #421 `lock_people` snapshots a catalog's owner before the catalog
    lock, so a concurrent transfer can still need a retry;
  - #423 provisioner every-use auditing (above);
  - #430 the owner view of the audit log exposes global entry ids, which
    show other teams' activity volume;
  - #434 flaky: the settings concurrent set/reset test;
  - #440 flaky: the passkey rename-and-remove journey;
  - from milestone 13: #361 (edit a suspended team's notice in the
    console), #381 (the team drawer's membership and delete controls are
    not guarded by a pending-write key) and #385 (the users and My keys
    drawers number their redraw after the list refresh).
- Open question for the maintainer, decided 2026-11-15 (issue #447): an
  unlock must be created by a credential whose user is an admin user;
  ending one needs only an admin credential on the catalog. Raised in
  review of PR #437.

# M14 Task 9c: the per-catalog maintenance flag and maintenance passes

Issue #407 (epic #390). Plan: `docs/superpowers/plans/2026-11-13-okf-mcp-m14-operator-api.md`, Task 9c.

## Added

- `okf-mcp catalog set-maintenance NAME on|off`, which prints `catalog C: maintenance OLD -> NEW`.
- `PUT /api/v1/catalogs/{catalog}/maintenance` with `{"maintenance": "on"|"off"}`, which answers `{name, previous, maintenance}`. An admin that reaches every catalog, or an owner of the catalog's owner team signed in to the console, may call it. Anyone else gets 403: a plain member, a scoped admin key, a provisioner, and a non-owner's session even for an unknown catalog.
- `store::catalogs::{Switch, MaintenanceChange, set_maintenance}`. A change records `catalog.maintenance_changed` (`{"from", "to"}`) through `record_catalog` in the same transaction and bumps the admin generation. Setting the flag a catalog already has records and bumps nothing. The owner check runs inside the transaction, in lock order: users through `audit::lock_people`, then the owner's `team_members` row `FOR SHARE`, then the catalog row `FOR NO KEY UPDATE` (where the owner team is checked again), then `server_state`.
- Migration `0017_maintenance_audit.sql`. It adds the index `maintenance_findings_run (catalog_id, run_id)`, so the entry's findings count reads one run's findings rather than every finding the catalog has had. It writes one `catalog.maintenance_run` entry (`run`, `state`, `findings`, source `system`, label `system:maintenance`) whenever a run leaves `running`, and also when a claim with nothing to do inserts its run already `completed`.
- `RunnerError::MaintenanceOff`, reason `maintenance_off`. `contract::result` maps a `Forbidden` refusal carrying `"maintenance": "off"` to it, and every other `isError` still maps to `Permission`.
- `GET /api/v1/catalogs` gains a `maintenance` field (`on` or `off`) on each catalog.

## Changed

- The runner skips a catalog that opted out. When its claim is refused this way it claims nothing and exits 0 with reason `maintenance_off`. It records no stop, so the pending claim replays with its idempotency key on the next run, and that run works normally once the flag is back on. If the flag is turned off after the claim, even with every submission acknowledged and only the close refused, the run stops with reason `maintenance_off` and exits 4: `RunSummary::exit_code` counts `maintenance_off` as success only when no run was claimed.
- `docs/operations.md`: the command row, the parity row, the API row, the endpoint counts (94 in total, 10 under Catalogs and import/export), both audit actions, the flag under "The maintenance runner", and the skip under "Report-only runner".
- `tests/admin_parity.rs` covers `catalog set-maintenance`.

## Notes

- The `catalogs.maintenance` column already came from `0016` (Task 9a). Migration `0017` was still needed, but only for the audit trigger.
- The trigger calls no audit resolver. `audit_credential`, `audit_person` and `audit_team_ref` lock users, and a run ends under its catalog's locks while `users::delete` holds the key's user `FOR UPDATE` before it deletes the key, which updates the run's `key_id`. That is a deadlock. Instead the trigger reads the run credential's `audit_credentials` row `FOR KEY SHARE SKIP LOCKED` (a row being forgotten is skipped) and reads the owner team's `audit_teams` id with a plain `SELECT`. The test `a_run_ends_without_waiting_for_its_users_or_credentials_rows` holds both rows and requires the close to finish within 30 seconds.
- No `Catalog.maintenance` field and no `teams::owns_catalog` were added; the plan has a dated correction.

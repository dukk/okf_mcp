# M21 Task 10: placement and the admin surface

## Added
- `src/stores/placement.rs`: `choose` (the owner team's pin, else the `default_store` setting), `default_store`, and `refuse_host_stores_until_task_20`, the one place that refuses a `host` store (plan decision P8; Task 20 deletes it and its one call).
- Admin API (`src/http/api/stores.rs`): `GET /api/v1/stores`, `POST /api/v1/stores`, `PUT /api/v1/settings/default_store`, `PUT` and `DELETE /api/v1/teams/{team}/store`. Reading needs an admin; changes need an admin reaching every catalog; no provisioner route.
- CLI (`src/cli/stores.rs`): `okf-mcp stores list`, `add`, `set-default`, `pin`, `unpin`. There is no removal command or route (YAGNI, per the plan).
- `store::stores::{add, set_default, summaries}`, `store::teams::set_store`, and `Stores::check`, the connection check, which runs the registry's own open path on a row that is not stored yet and closes the pool again.
- Audit actions `store.added` (target kind `store`), `setting.default_store_changed` and `team.store_pinned` / `team.store_unpinned`. Details never carry a host, port or credential reference.
- Docs: "Catalog stores API" and the new audit rows in `docs/operations.md`.

## Changed
- `catalogs::create` calls `placement::choose` and records the store on the new row. Existing catalogs never move: changing the default or a pin affects only catalogs created afterwards.
- `Stores::open` is split: reading the row, then `open_row`, shared with `check`. `Stores::with_environment` is `pub(crate)` so tests can supply a credential without touching the process environment.
- `settings::updated_by` is `pub(crate)` (the default-store write reuses it).

## Notes
- Every transaction that changes a `stores` row, the default or a pin sends an invalidation (Task 8's requirement): `Invalidation::Store` for a new row, `Invalidation::Settings` for the default, `Invalidation::Team` for a pin. An unchanged default or pin writes, records and sends nothing.
- `add` checks the connection first and holds no transaction while it does, so no account connection is held while a store pool connects. A store that cannot be reached is refused with a message that names no host, port or credential; the cause is logged locally only. Because the check comes before the P8 refusal, an unreachable `host` store is refused as unreachable and a reachable one as "not enabled yet". Until Task 20 no store can actually be added, so the audit and invalidation of `store.added` are covered by testing the unexported `insert` that `add` ends in.
- `stores` is not in the CLI/API/console parity test's groups, so no console control was built.
- No migration: 0038 already widened the audit checks.
- Plan: a dated correction note on Task 10 and a changed Task 20 step; comments on #969 (Task 10b) and #979 (Task 20).

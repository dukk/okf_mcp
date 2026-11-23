# M21 Task 2: catalog writes take `CatalogDb`

## Added
- Nothing new: this task only retypes entry points.

## Changed
- `store::import::import` and `store::repair::repair` take `&CatalogDb` instead of `&PgPool`, and begin their transaction with `CatalogDb::begin`.
- `store::publish::publish`, `write::apply` and `write::apply_audited` (every changeset edit and the revert tool), `open_changeset` and the discard path in `mcp::changesets` begin their transaction on `App::main_store()` instead of `App::pool()`.
- Callers: the CLI (`import`, `repair-indexes`), the HTTP import route (`App::main_store()`), and the tests.
- `CatalogDb::new` is `pub` (it was `pub(crate)`), so integration tests that hold only a pool can build the release-1 handle with `CatalogDb::new(StoreName::main(), pool.clone())`. The server and its request paths get their handle from `App`. The CLI is the release-1 exception: `import` and `repair-indexes` (`src/cli.rs`) build their own main-store handle from the pool they connect with, since they run without an `App`. Task 8 or later work that routes by store must migrate those two constructions too.

## Notes
- No behaviour change: in release 1 `main_store()` and the account pool are the same pool. Reads inside these functions (`state.refresh`, `changesets::entries`, `with_current_revisions`) still use `App::pool()`; Task 3 types them.
- **Crosses in release 1.** Statements inside the typed transactions that still touch account tables, the inventory Tasks 11-13 work from (line numbers at this commit):
  - Catalog registry row (`catalogs`, an account table until Task 11):
    - DONE in Task 11 (issue #970): `UPDATE catalogs SET generation = generation + 1` in publish, import and repair is now `catalogs::bump_generation` on `catalog.catalog_state`;
    - DONE in Task 11: `SELECT generation FROM catalogs` in `current_base` reads `catalog_state`;
    - DONE in Task 11: `catalogs::lock_by_name` finds the catalog on the account transaction, takes the advisory lock on the store transaction and reads the generation from `catalog_state`. `catalogs::lock` (the `pg_advisory_xact_lock` on the catalog id) touches no table and stays as it was;
    - DONE in Task 11: `changesets::lock_catalog` locks the `catalog_state` row (`FOR KEY SHARE`, and `deleted_at IS NULL`).
  - Users and teams: `audit::lock_people` (`src/store/audit.rs`, with a `People { catalogs: [name] }`), called through `publish::lock_people` from publish, `write::apply`/`apply_audited` and the discard path, and directly from `src/store/import.rs` and `src/store/repair.rs`. One `SELECT` reads `users`, `api_keys` and `sign_in_codes` (the actor's credential owner), `teams` and `catalogs` (the catalog's owner team and its personal person), then each `users` row is locked `FOR KEY SHARE` in id order. It reads no audit table.
  - Audit writes: every one goes through `audit::record`, and `catalogs::record_catalog` (import, repair) or the caller (publish, `write::apply_audited` revert, discard) first calls `audit::catalog_teams`.
    - `audit::catalog_teams` reads `catalogs` (`owner_team_id`) and calls the SQL function `audit_team_ref`, which reads `teams` and reads, refreshes or inserts `audit_teams`.
    - `audit::record` inserts into `audit_entries`. Its statement reads `users`, `teams`, `api_keys` and `sign_in_codes`, and calls `audit_person`, `audit_team_ref` and `audit_credential`, which lock `users` `FOR KEY SHARE`, read `api_keys` and `sign_in_codes`, and read, refresh or insert `audit_people`, `audit_teams` and `audit_credentials`.
    - Sites: `src/store/publish.rs` (published), `src/store/import.rs` (imported), `src/store/repair.rs` (repaired), `src/write.rs` (reverted) and `src/mcp/changesets.rs` (changeset discarded).
  - Cache signalling: `invalidate(..)` sends `pg_notify` on the one channel in publish, import and repair (it touches no table for a catalog invalidation); Task 17.
  - DONE in Task 11: foreign keys `changesets.catalog_id -> catalogs`, `changesets.key_id -> api_keys` (written by `changesets::open`), and `documents`, `revisions` -> `catalogs`: the catalog ones point at `catalog.catalog_state`, the `key_id` one is dropped (migration 0041).
- Not in this task's entry points: `src/store/changesets.rs:654` (the listing join with `catalogs` and `api_keys`) and `src/store/repair.rs:281` (`missing_root_index`) are reads and belong to Tasks 3 and 5.
- Tests: the existing suite, with handle types changed.

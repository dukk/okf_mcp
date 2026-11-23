# M21 Task 2: catalog writes take `CatalogDb`

## Added
- Nothing new: this task only retypes entry points.

## Changed
- `store::import::import` and `store::repair::repair` take `&CatalogDb` instead of `&PgPool`, and begin their transaction with `CatalogDb::begin`.
- `store::publish::publish`, `write::apply` and `write::apply_audited` (every changeset edit and the revert tool), `open_changeset` and the discard path in `mcp::changesets` begin their transaction on `App::main_store()` instead of `App::pool()`.
- Callers: the CLI (`import`, `repair-indexes`), the HTTP import route (`App::main_store()`), and the tests.
- `CatalogDb::new` is `pub` (it was `pub(crate)`), so integration tests that hold only a pool can build the release-1 handle with `CatalogDb::new(StoreName::main(), pool.clone())`. Production code still gets its handle from `App`.

## Notes
- No behaviour change: in release 1 `main_store()` and the account pool are the same pool. Reads inside these functions (`state.refresh`, `changesets::entries`, `with_current_revisions`) still use `App::pool()`; Task 3 types them.
- **Crosses in release 1.** Statements inside the typed transactions that still touch account tables, the inventory Tasks 11-13 work from (line numbers at this commit):
  - Catalog registry row (`catalogs`, an account table until Task 11):
    - `UPDATE catalogs SET generation = generation + 1` in `src/store/publish.rs` (`publish_locked`), `src/store/import.rs` (`import`) and `src/store/repair.rs` (`repair`);
    - `SELECT generation FROM catalogs` in `src/store/publish.rs` (`current_base`);
    - `catalogs::lock_by_name` -> `find` reads `catalogs` by name (`src/store/import.rs`, `src/store/repair.rs`);
    - `changesets::lock_catalog`, `SELECT id FROM catalogs ... FOR KEY SHARE` (`src/store/changesets.rs:241`), called from `src/store/publish.rs`, `src/write.rs` and `src/mcp/changesets.rs` (open and discard).
  - Users and teams: `audit::lock_people` (locks `users`, `teams`, `catalogs` and reads the audit reference tables) in `src/store/publish.rs` (`lock_people`, called from publish, `write::apply`, discard), `src/store/import.rs` and `src/store/repair.rs`.
  - Audit writes: `audit::catalog_teams` (reads `catalogs`, `audit_team_ref`) and `audit::record` (account `audit_entries`, `audit_people`, `audit_teams`, `audit_credentials`) in `src/store/publish.rs` (published), `catalogs::record_catalog` from `src/store/import.rs` (imported) and `src/store/repair.rs` (repaired), `src/write.rs` (`apply_audited`, reverted) and `src/mcp/changesets.rs` (changeset discarded).
  - Cache signalling: `invalidate(..)` (`pg_notify` on the one channel) in publish, import and repair; Task 17.
  - Foreign keys: `changesets.catalog_id -> catalogs`, `changesets.key_id -> api_keys` (written by `changesets::open`), and `documents`, `revisions` -> `catalogs`; Task 11 removes them.
- Not in this task's entry points: `src/store/changesets.rs:654` (the listing join with `catalogs` and `api_keys`) and `src/store/repair.rs:281` (`missing_root_index`) are reads and belong to Tasks 3 and 5.
- Tests: the existing suite, with handle types changed.

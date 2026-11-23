# M21 Task 3: catalog reads take `CatalogDb`

## Added
- `CatalogDb::main_of(&pool)`, a test-only constructor for the release-1 `main` handle around a pool a unit test holds.

## Changed
- `CatalogState::load`, `CatalogState::refresh` and `CatalogState::current_covering` (snapshot loads and refreshes) take `&CatalogDb` instead of `&PgPool`; so does the `build` function behind them.
- `CatalogCache` holds a `CatalogDb` (`CatalogCache::new(db, ..)`), the clone of `App`'s `main_store` that `App::new` hands it.
- `OverlayCache::get` takes `&CatalogDb` (it reads the changeset's entries and calls `current_covering`).
- `history::revisions`, `history::content_at` and `history::recent` (history, revision content, the console's recent-revisions page) take `&CatalogDb`.
- `export::export` takes `&CatalogDb`; its private `record_export` takes it too.
- Callers get the handle from `App::main_store()`: `mcp::history`, `mcp::changesets` (revert's `content_at`, `pending_changes`), `mcp::context` (`ToolContext::view`, `resolve_changeset`'s `changesets::by_handle`), `http::api::browse`, `http::archives` (the export route), `write::apply` and `store::publish::publish` (their `state.refresh`, and `publish`'s `changesets::entries`).
- `publish::with_current_revisions` takes `&CatalogDb`.
- The CLI's `export` builds its own main-store handle from the pool it connects with, as its `import` and `repair-indexes` do (Task 2). It is the one production path here that does not get its handle from `App`; Tasks 8 and 10 must route it through the registry.
- Tests: handle types only (`CatalogDb::main_of` in unit tests; `CatalogDb::new(StoreName::main(), pool.clone())` in the integration tests under `tests/`).

## Notes
- No behaviour change: in release 1 `main_store()` and the account pool are one pool.
- Entry points that read only store tables and so cross nothing: `history::revisions`, `content_at` and `recent` (`revisions`); `with_current_revisions` (`documents`); `changesets::entries` (`changeset_entries`); `changesets::by_handle` (`changesets`); `documents::published` (`documents`, taken as an executor, so P2 applies to it).
- **Crosses in release 1**, the reads typed above that still touch account tables (additions to Task 2's inventory in `changelog/2026-11-23T12-55-43Z-m21-task-02-catalog-writes-take-catalogdb.md`; each line read from the code at this commit):
  - Snapshot load and refresh (`src/catalog.rs`):
    - `build` (called by `CatalogState::load` and, through `refresh_to`, by `refresh`) opens one `REPEATABLE READ, READ ONLY` transaction on `db.executor()` and runs `SELECT generation FROM catalogs WHERE id = $1` (the `GENERATION` constant, an account table) and then `documents::published` (`documents`) in it. The generation and the rows agree because both tables are in one database; Task 11 must keep that guarantee when the generation moves to the store.
    - `generation_in` runs the same `GENERATION` statement on `db.executor()` in `CatalogState::refresh`, and on the caller's transaction in `refresh_locked`, which only `publish::current_base` calls (under the catalog's advisory lock). Task 2's list names `current_base`'s own `SELECT generation FROM catalogs` but not this second read, nor the `documents` read `refresh_locked` makes through `documents::published`.
    - Callers of these: `CatalogCache::get` and `catch_up` (the latter also from `App` and `http::archives` after an announcement), `write::apply`, `publish`, `OverlayCache::get` (from `ToolContext::view`) and `pending_changes`.
  - Export (`src/store/export.rs`), `export`'s snapshot transaction (`db.begin()`, then `SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY`):
    - `catalogs::find` runs the `select_catalogs!` statement: `catalogs`, with a subquery on `teams` for the owner team's name;
    - a statement joining `catalogs` and `audit_teams` reads the owner team's audit id;
    - `documents::published` (`documents`).
    - The first two read account tables inside a transaction that also reads `documents`.
  - Export's audit record (`record_export`, a second transaction on `db.begin()`), the `catalog.exported` entry that P4 keeps in the account database:
    - `audit::lock_people` with an empty `People`: one `SELECT` naming `users`, `api_keys`, `sign_in_codes`, `teams` and `catalogs`, then each `users` row it found is locked `FOR KEY SHARE`;
    - `audit::record`: inserts into `audit_entries`; as in Task 2's inventory its statement reads `users`, `teams`, `api_keys` and `sign_in_codes` and calls `audit_person`, `audit_team_ref` and `audit_credential`.
    - Before Task 11 moves the catalog tables out of the account schema, `record_export` and the two reads above must be given an `&AccountDb` (or the registry read split from the `documents` read).
- Still on `App::pool()`, not part of this task:
  - the changeset listing in `mcp::changesets` (`changesets::list_open` and `open_for_catalogs`, which join `catalogs` and `api_keys`), and the table reads in `src/store/changesets.rs`, `src/store/repair.rs` and `src/store/users.rs` that Task 5 splits;
  - `ToolContext::resolve_changeset`'s `catalogs::row_by_id` (the registry, an account read);
  - `src/store/maintenance/*` and `src/mcp/maintenance/*` (Task 4).
- Files named by the issue that needed no change: `src/store/documents.rs` (executor-based helpers), `src/store/gates.rs` (reads only account tables: `team_members`, `user_catalogs`, `unlocks`, `audit_people` and `audit_credentials`), `src/mcp/documents.rs` and `src/mcp/search.rs` (they read the in-memory snapshot and reach the database only through `ToolContext`, which this task types).
- I traced the statements above in the code; I did not trace the SQL functions beyond the names Task 2 already lists.

> **Update (issue #962, 2026-11-23).** The maintenance modules named under "Still on `App::pool()`" above moved to `&CatalogDb` in Task 4; see `changelog/2026-11-23T14-04-24Z-m21-task-04-maintenance-takes-catalogdb.md`.

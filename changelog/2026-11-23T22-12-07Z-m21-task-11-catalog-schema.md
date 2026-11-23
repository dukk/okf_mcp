# M21 Task 11: schema `catalog`, `catalog_state`, foreign keys, and the lock changes

Issue #970. Migration `0041_catalog_schema.sql`. The catalog's own tables
leave `public`; store pools see only schema `catalog`, account pools only
`public`.

## NOT ROLLING-SAFE (plan decision P10)

Release-2 replicas query these tables in `public` and read
`catalogs.generation`; this migration removes both and takes ACCESS
EXCLUSIVE locks. It belongs to the release-3 maintenance window: snapshot,
scale `serve` to zero, `okf-mcp migrate`, start release 3.

## Added

- Migration 0041: schema `catalog`; `catalog.catalog_state(id, generation,
  deleted_at)` seeded for every provisioned catalog and every half-created
  one that already has documents (a half-created one with none gets no row,
  so recovery still finds its store side missing); eleven tables moved with
  `SET SCHEMA`; ten catalog-id foreign keys re-pointed to `catalog_state`
  (ON DELETE CASCADE); `changesets_key_id_fkey` and
  `maintenance_runs_key_id_fkey` dropped; the maintenance audit triggers and
  `audit_maintenance_run()` dropped; `catalogs.generation` dropped;
  `catalogs.deleted_at` and `store_cleanups` added.
- `STORE_SEARCH_PATH` (`catalog`), set in the pool's `after_connect` hook so
  a credential refresh that replaces the connect options keeps it.
- `RunWatch` (maintenance) records `catalog.maintenance_run` from Rust after
  the store commit, for both paths the triggers covered.
- Tests: `no_foreign_key_crosses_between_schemas`,
  `migration_0041_keeps_every_row_and_seeds_the_state_rows`,
  `a_store_pool_cannot_see_account_tables`,
  `an_account_pool_cannot_see_catalog_tables`,
  `publish_bumps_generation_in_catalog_state`,
  `a_store_keeps_its_search_path_after_a_credential_refresh`,
  `maintenance_run_end_is_audited_after_the_trigger_is_gone`,
  `an_empty_maintenance_run_is_audited_after_the_trigger_is_gone`,
  `a_changeset_opened_while_a_deletion_runs_is_left_open_under_a_deleted_key`.

## Changed

- Generation is read and bumped on `catalog.catalog_state`. The account SQL
  returns `0::bigint`; `CatalogDirectory::fill_generations`/`with_generation`,
  `lock_by_name`, export and `catalogs::list_in` fill it from the store.
- `changesets::lock_catalog` takes `FOR KEY SHARE` on `catalog_state`.
- `CatalogDb` carries the account pool (`CatalogDb::main_in` builds the
  handle for `main`); `catalogs::register` is `pub(crate)` and runs on an
  account connection; `store_side_exists` and `write_starter_files` use
  `catalog_state` (Task 10b's hand-offs).
- Interim dual-transaction pattern until Tasks 12 and 13: acquire the
  account connection first, then the store's; `lock_people` (account), then
  the advisory lock (store), then rows; commit the store first, then the
  account (`catalogs::delete_on` commits the account first). `invalidate`
  rides the account transaction. With one pool (`database_max_connections =
  1`) no transaction on one handle waits for a connection on the other.
- `users::delete` uses `main_in` until Task 13. The "crosses in release 1"
  items this covers are removed from the Task 2, 3 and 4 changelogs.

## Pools (review of PR #1001)

`CatalogDb::main_in`, which builds a pool of its own, is test-only now.
Every production path reaches the `main` store through the registry's one
pool for it: the server's handlers use `app.stores()`/`store_db`,
`CatalogDirectory::new` takes the `Stores` (no fallback handle),
`users::delete_on` and `delete_own_on` take the store handle (the
command line gets it from a `Stores` it builds once per command), and the
HTTP catalog deletion uses `catalogs::delete_in`. The pool-of-one test is
`a_one_connection_configuration_never_opens_a_second_pool_for_main`
(create, import, directory load and delete on `database_max_connections = 1`,
and the registry opens one store pool). The test-only forms are
`catalogs::{create, delete}` and `users::{delete, delete_own}`; the
integration tests use `tests/support/stores.rs`.

## Evidence

Gates on this change (`tools/agentic/gate.sh`: fmt, lint, dylint, test) ran
green; the named tests, each seen to fail with its change reverted, are
`maintenance_run_end_is_audited_after_the_trigger_is_gone`,
`an_empty_maintenance_run_is_audited_after_the_trigger_is_gone`,
`publish_bumps_generation_in_catalog_state`,
`a_store_pool_cannot_see_account_tables`,
`an_account_pool_cannot_see_catalog_tables` and
`no_foreign_key_crosses_between_schemas`.

## Known gaps (Task 13)

- A key id left on a changeset or run after its key is deleted is now a
  plain value (no foreign key): readers show no key name. `store_cleanups`
  cleanup arrives in Task 13.
- A user deletion no longer waits for an in-flight changeset open; the
  changeset stays open under a deleted key until Task 13's interlock.

## Test support

`TestDb::store()` (the store-side pool of a test database) and
`catalogs::test_generation`.

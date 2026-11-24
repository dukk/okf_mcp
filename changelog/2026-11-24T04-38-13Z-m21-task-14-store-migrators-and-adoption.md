# M21 Task 14: two migrators, the catalog baseline, adoption, `stores migrate`

Issue #973. Every store's `catalog` schema now has a migration history of
its own, in `catalog._sqlx_migrations`. A new store is built from a
baseline; the account database, whose catalog schema the account migrations
0041 and 0042 built, adopts that baseline instead of running it. Each
database records the one store it belongs to.

## Added

- `migrations/catalog/0001_baseline.sql`: the `catalog` schema exactly as
  0041 and 0042 leave it, generated from a schema-only dump of a database
  migrated through 0042 and rewritten as readable SQL. When it was written,
  a pg_dump 17 schema dump of a database built from it matched the original
  dump; `adopted_schema_equals_a_fresh_store_schema` keeps checking the
  schemas are equal, through catalog queries.
- `migrations/catalog/0002_store_identity.sql`: `catalog.store_identity`,
  at most one row, the name of the store the database belongs to.
- `db::CATALOG_MIGRATOR` (history in `catalog._sqlx_migrations`) and
  `db::migrate_store(conn, store) -> StoreMigration`. When a store is already
  current, the check is reads only. Otherwise the work runs under the
  advisory lock `migrate` uses: create schema `catalog` if absent, adopt
  (when `catalog_state` exists and the history is empty, record the
  baseline with its embedded checksum without running it), run the rest,
  then write the identity row if absent and refuse one that names another
  store.
- `db::store_owner` and `db::holds_account_tables`, the two reads behind the
  identity checks.
- `Stores::migration(store)` (what the store's first open did) and
  `Stores::prepare(row)` (migrates a store that is about to be added).
- `okf-mcp stores migrate`: migrates every store in name order and prints
  one line each. A store that cannot be reached or migrated is reported and
  the rest still run; the command then exits with an error that counts the
  failures.
- Tests: `adopted_schema_equals_a_fresh_store_schema`,
  `adoption_runs_once_under_concurrent_starts`,
  `a_store_opened_under_another_name_is_refused_and_keeps_its_identity`
  (`src/db.rs`); `a_fresh_store_gets_the_baseline`,
  `a_store_whose_database_belongs_to_another_is_unavailable_and_others_still_open`
  (`src/stores/registry.rs`); `adding_a_store_that_aliases_another_is_refused`,
  `adding_the_account_database_as_a_host_store_is_refused`
  (`src/store/stores.rs`); `migrate_stores_reports_an_unreachable_store_and_continues`
  (`src/cli/stores.rs`).

## Changed

- `db::migrate` (every server start and `okf-mcp migrate`) adopts the
  account database as store `main` after the account migrations, holding
  the same lock on the same connection, so a pool of one connection still
  works.
- `Stores::get` migrates a store the first time its pool opens in the
  process, on the store's own pool. Its credential task starts after that.
  A failed migration, or an identity row naming another store, is
  `StoreUnavailable` (logged locally with the store's name and the identity
  found). The pool is closed and nothing is cached, so other stores are
  unaffected and the next request tries again.
- `Stores::check` (adding a store) also refuses, with reads only, the
  account database reached under another host name, and a database whose
  identity row names another store. `stores::add` runs check, then the P8
  refusal, then the migration, then the insert. While P8 stands, no add
  migrates anything.
- `tests/support/pg.rs`: the migrated template adopts the baseline and runs
  the later catalog migrations, but writes no identity row, so a copy can be
  opened as any store. Its name hashes the catalog migrations too.
- `build.rs`'s note: the watched `migrations` folder covers both sets.

## Notes

- The plan put `store_identity` in the baseline. It is its own migration,
  because adoption records the baseline without running it, and the
  baseline has to equal what 0041 and 0042 built.
- Schema-qualified names appear in `src/db.rs`'s migration code (the
  account database's migration connection has `search_path = public`).
  P1's planned guard test should exempt it.
- The registry lookup on the bare account pool (#1006) is unchanged. The
  new step runs on the store's pool, after the lookup.
- Plan corrections: the dated note under Task 14, with follow-ups for
  Tasks 15 and 20.

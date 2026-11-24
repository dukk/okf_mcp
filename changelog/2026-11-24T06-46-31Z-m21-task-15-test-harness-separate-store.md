# M21 Task 15: the test harness runs catalog tests on a separate store

Issue #974, Task 15 of `docs/superpowers/plans/2026-11-23-okf-mcp-catalog-stores.md`.

## Added

- `OKF_TEST_STORE=same|separate` in `tests/support/pg.rs` (`StoreMode`). `same` (the default) keeps the
  catalog data in the account database. `separate` copies a second database, `<account database>_store`, from a
  store template and registers it as the store `main` with location `host` and an `env` credential
  (`OKF_TEST_STORE_PASSWORD`, set once per process from the server URL). The template has the catalog schema
  and no identity row, as `db::migrate_store` leaves it; the first open of the store writes the identity.
- `TestDb::extra_store_database(suffix)`: another store database for a test, copied from the same template.
- Test helpers `crate::db::store_pool`, `crate::db::catalog_id`, `crate::db::main_in_the_account_database`,
  `pg_support::harness_password`, `pg::store_pool` and `pg::store_options`.
- Test `isolation_a_catalog_on_store_b_writes_nothing_to_store_a` (`src/stores/placement.rs`).

## Changed

- `just test` (so `tools/agentic/gate.sh test`) runs the suite twice in one container, `same` then `separate`;
  both runs happen and the recipe fails if either did. The gate timeout is unchanged. README and the gate
  header say so.
- Tests that read or wrote `catalog.*` through the account pool now use the store's pool. Statements that joined
  the registry (`catalogs`, `users`, `audit_*`) to `catalog.*` became two queries. Lock-wait helpers see the
  store database (a prefix match on the database name). `fail_on` and `allow` install a trigger on the store for
  a `catalog.*` table. Tests of the release-one layout (the `main` row is the account database) first put
  `main` back on it. A test registry's environment includes the harness password.
- `the_maintenance_run_entrys_findings_count_reads_one_run_by_index` drops the other `catalog_id` index in its
  transaction: on an empty table the planner's choice between the two follows index creation order, which
  differs between the account migrations and the catalog baseline.
- Plan Task 15 and open question 1 are corrected with what later tasks need.

## Notes

- Run times, warm cache, on a shared machine, one `cargo test --no-fail-fast` each: `same` about 242 s
  (lib 77 s of it); `separate` about 75 s for the lib and 114 s for the integration binaries. Both modes fit the
  75 minute gate limit many times over, so no binary is left out of `separate`.
- Every test that failed only in `separate` mode assumed one database (catalog statements on the account pool,
  a cross-database join or subselect, a lock-wait on the account database's name, or a registry whose
  environment lacked the store credential). None was a production bug.
- `tests/scale.rs` (ignored) was changed to seed documents through the store; it was not run.

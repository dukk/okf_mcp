# M21 Task 20: end-to-end test, smoke test, P8 lifted (issue #979)

## Added

- `tests/stores.rs`, `two_stores_serve_their_catalogs_and_a_lost_store_costs_only_its_own`: a server over the account database and two stores (`main` and `acme`, a `host` store added through `stores::add`); team A pinned to `acme`; a catalog for each of teams A and B. It runs open, edit, publish, history, revert, a search across both catalogs, export and a maintenance claim and submit; checks each catalog's rows exist only on its own store (documents and revisions); restarts the server so it holds no loaded catalog state; stops `acme`'s database; and asserts team B reads and writes as before while team A's `open_changeset` is `StoreUnavailable` and a search across both names `alpha` in `unavailable` and still returns `beta`.
- `tests/stores.rs`, `the_upgrade_procedure_adopts_a_pre_split_database_and_keeps_its_history`: the smoke test of the release-3 procedure (see Notes).
- `tests/support/stores.rs`: `StoreProxy`, a forwarding TCP proxy that stands for the store's database and stops it. `tests/support/server.rs`: `TestServer::start_on` (serve a database the test built) and `create_owner`.
- `repair::placed_on`, which routes the catalog names given to `catalog repair-indexes` to the store that holds each.
- Unit tests: `a_host_store_can_be_added` (replaces `adding_a_host_store_is_refused_until_enabled`; it checks the new store's database has the embedded catalog migrations and an identity row naming it), `a_store_whose_schema_cannot_be_migrated_leaves_no_row`, `repair_indexes_repairs_a_catalog_on_each_store`.

## Changed

- P8 is lifted: `placement::refuse_host_stores_until_task_20` and its call in `store::stores::add` are deleted (with its unit test), so a `host` store can be added through the API and the CLI. "Catalog stores" and "Catalog stores API" in `docs/operations.md` no longer say host stores are refused.
- `okf-mcp catalog repair-indexes` visits every store (`Stores::all()`) and repairs each catalog on the store that holds it; `repair::missing_root_index` lists only the catalogs placed on the store it is given. Before this, an earlier task had left it on `main` only (#966). A store that fails is reported at the end; the others still run.
- Plan Task 20 is ticked and carries a dated correction note.

## Notes

- **Smoke test.** Run as a test, on a disposable database in the test harness, never on anything real. The database is built at the schema release 2 has (the account migrations 1 to 35, byte-identical to `migrations/` on `origin/main`), seeded with two catalogs, their documents and revisions and one catalog audit entry in the account table; then `okf-mcp migrate` (through `cli::run`) and the registry's migration of `main` (what `stores migrate` does) run. Result: `catalog.store_identity` names `main`; every catalog migration is recorded; the two catalogs, documents and revisions are in schema `catalog` and `public.documents` is gone; a server on it publishes to an old catalog; and the audit log lists the old entry (no `store`) and the new one (`store: main`), each once, newest first. A smoke test against a real snapshot of the maintainer's database is still for the maintainer.
- `cli::run` installs the telemetry subscriber once per process, so a test can run one subcommand through it.
- The end-to-end outage assertions run after a restart: a loaded catalog state is served from memory, so a merged search over it would still return it.
- Each new assertion was checked to fail with its behaviour reverted: the refusal restored (the add, the CLI test and the end-to-end test fail), the CLI loop cut back to `main` (the CLI test fails), and the proxy left running (the end-to-end test fails on `open_changeset`).
- Known flakes that are not this task's: #1002 (28P01 password rotation), #1004.

The milestone summary is `2026-11-24T17-02-21Z-m21-milestone-catalog-stores.md`.

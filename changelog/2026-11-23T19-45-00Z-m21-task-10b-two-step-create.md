# M21 Task 10b: creating a catalog in two steps

## Added
- `catalogs::create` is now three steps (plan decision P9): `register` inserts the registry row with `provisioned_at` NULL (store from `placement::choose`, in the same transaction) and records `catalog.created`; `provision_store_side` takes the catalog's lock on the store, checks that the registry row still exists and is unprovisioned, and writes the starter files; `mark_provisioned` sets `provisioned_at` and sends the invalidations.
- `catalogs::{pending, recover, Pending, Recovery}` and `store_side_exists`, the one function that says whether a catalog's store side is there (Task 11 adds `catalog_state` to it).
- Housekeeping task `provision_catalogs` (every minute): for each unprovisioned row older than a minute it takes the catalog's lock, then finishes the catalog if its store side exists, or records `catalog.deleted` with `failed_create: true` in the details and removes the row. Metrics and spans follow the other tasks (`okf_housekeeping_runs_total{task="provision_catalogs"}`).
- Tests for the five named cases, a one-connection pool; `a_half_created_catalog_is_not_visible` checks a row made by `register`. The two concurrency tests use `Notify` and wait on `pg_locks`, never a sleep.
- Docs: "Half-created catalogs" in `docs/operations.md`.

## Changed
- Every account-side reader already hides a half-created row: the filters shipped first, in release 2, as Task 10b part 1 (PR #1000, decision P11); see `changelog/2026-11-23T20-20-27Z-m21-task-10b-part-1-hide-unprovisioned-readers.md`. This change only starts writing such a row.

## Notes
- No migration: 0040 has the unprovisioned index.
- `store_side_exists` takes a store connection, not the `CatalogDb` the plan names: the check runs inside the transaction that holds the lock, and a second connection from a pool of one would never be returned. Until release 3 the store is the account database, so the same connection also reads the registry row.
- Ships as release 2b, after release 2 is on every replica (P11).

# M21 Task 10b: creating a catalog in two steps

## Added
- `catalogs::create` is now three steps (plan decision P9): `register` inserts the registry row with `provisioned_at` NULL (store from `placement::choose`, in the same transaction) and records `catalog.created`; `provision_store_side` takes the catalog's lock on the store, checks that the registry row still exists and is unprovisioned, and writes the starter files; `mark_provisioned` sets `provisioned_at` and sends the invalidations.
- `catalogs::{pending, recover, Pending, Recovery}` and `store_side_exists`, the one function that says whether a catalog's store side is there (Task 11 adds `catalog_state` to it).
- Housekeeping task `provision_catalogs` (every minute): for each unprovisioned row older than a minute it takes the catalog's lock, then finishes the catalog if its store side exists, or records `catalog.deleted` with `failed_create: true` in the details and removes the row. Metrics and spans follow the other tasks (`okf_housekeeping_runs_total{task="provision_catalogs"}`).
- Tests for the five named cases, a one-connection pool, and the hidden readers below. The two concurrency tests use `Notify` and wait on `pg_locks`, never a sleep.
- Docs: "Half-created catalogs" in `docs/operations.md`.

## Changed
- A half-created row is hidden from every account-side reader that could show or act on it: `catalogs::find` and `list` (so every by-name change, delete and lock), the ceiling's named, highest, page and entitlement reads and its by-name writes, a team's catalog list, a key's catalog names, and `repair::missing_root_index`. Readers by id (unlocks, sign-in codes, key and changeset joins, audit rendering, labels) are reached only through ids handed out for provisioned catalogs, so they are unchanged.

## Notes
- No migration: 0040 has the unprovisioned index.
- `store_side_exists` takes a store connection, not the `CatalogDb` the plan names: the check runs inside the transaction that holds the lock, and a second connection from a pool of one would never be returned. Until release 3 the store is the account database, so the same connection also reads the registry row.
- Ships as release 2b, after release 2 is on every replica (P11).

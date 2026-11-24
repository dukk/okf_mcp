# M21 issue 1028: half-created catalog recovery is bounded

Brooke's third review of the milestone PR found that `provision_catalogs`
waited without limit on a catalog advisory lock another connection held, on a
store query that stalled, or on a store's first open and migration. One such
wait held up every catalog behind it, on healthy stores too.

## What changed

- `catalogs::recover` takes a `wait` and runs its account and store
  transactions through `account_tx` and `store_tx`, so `lock_timeout` and
  `statement_timeout` are set for the transaction (the advisory lock wait
  included).
- `provision_catalogs` bounds each attempt with `backstop`: the store open
  (registry read, connect, first-open migration) and the recovery together.
- A timeout (`stores::bounded::is_timeout`: `Busy` from the backstop, or
  SQLSTATE `55P03` / `57014` from the database) is contained like
  `StoreUnavailable`: the store is skipped for the rest of the run, the row
  stays pending, the first failure is the task's result.

## Why these bounds

No new setting: `wait` is `store_connect_timeout` (as the registry read uses),
the attempt backstop six times it, 30 s by default, the same as `LIMITS`.
The database bound is the shorter, so PostgreSQL gives up and rolls back
first; dropping the future alone would not cancel a statement already sent.
`lock_timeout` on `pg_advisory_xact_lock` was chosen over
`pg_try_advisory_xact_lock` because it bounds every other wait in the
transaction (user rows, table locks) the same way with one setting.

## Tests

`a_held_catalog_lock_does_not_starve_a_healthy_store`,
`a_stalled_store_query_does_not_starve_a_healthy_store` and
`the_attempt_backstop_contains_a_wait_the_database_does_not_bound`
(`src/housekeeping/store_tests.rs`). Each holds a lock on the older catalog's
store with a second connection and asserts the newer catalog on a healthy
store settles before it is released. No sleeps; a test-level timeout turns a
hang into a failure.

# M21 Task 16: per-store housekeeping (issue #976)

## Added

- Per-store housekeeping jobs, keyed `<task>:<store>:<slot>`: `prune_changesets`,
  the store half of `prune_audit` and the open changeset count of `cluster_stats`
  run once per store and slot across the cluster. The ticker reads the `stores`
  table on every pass, so a store added while the replicas run gets its jobs at
  the next pass.
- Account migration `0043_cluster_store_stats.sql`: each store's last open
  changeset count. The cluster `cluster_stats` job sums it into
  `cluster_stats.open_changesets`; a store that cannot be reached keeps its last
  count, and its row goes with the store.
- `src/stores/bounded.rs`: the bounded-wait helpers (`Limits`, `LIMITS`,
  `backstop`, `account_tx`, `store_tx`, `bound_waits`), shared by the store
  cleanup worker, the housekeeping jobs and the registry.
- Tests in `src/housekeeping/store_tests.rs` (two replicas over one account
  database with two separate stores; a dropped store database; the summed
  count; a store added at run time; a held lock), and #1006 regression tests in
  `src/stores/registry.rs` and `src/store/cleanups_tests.rs`.

## Changed

- `housekeeping::schedule` takes a `&Housekeeping` job; `Housekeeping` carries an
  optional `store`, and `run_on_store` runs a job's store half. A store job's
  own prune, count and write statements run under `lock_timeout` and
  `statement_timeout` (20 s). Each step of a job (opening the store, the
  prune, the store count, the account write) also has its own 30 s `tokio`
  backstop, so a job with several steps can take longer than 30 s in all,
  but no single step can. Three things are bounded differently: the registry reads that find and open
  the store use `store_connect_timeout`, a store's first-open migrations have
  only the backstop, and `prune_audit`'s settings read uses its own timeout.
  No job holds an account and a store transaction at once.
- `audit::prune_catalog` takes a `wait` bound; `changesets::prune_empty` and
  `changesets::open_total` take any executor.
- Issue #1006: `Stores::all` and the registry row read on a store's first open
  run in a bounded account transaction (`store_connect_timeout`), and the
  cleanup worker puts each store's open under its backstop, so a locked
  `stores` table no longer stalls the run.
- `docs/operations.md`: the housekeeping and cluster gauge notes describe the
  per-store jobs.

## Notes

- A job whose store has no row does nothing and succeeds, so it is not retried.
  An old pre-store `prune_changesets:<slot>` job left in the queue at upgrade
  does nothing; its store jobs do the work.
- `okf_changesets_open` lags each store's count by at most one `cluster_stats`
  cadence (a minute).

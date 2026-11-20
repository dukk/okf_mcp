## Added
- Nothing.

## Changed
- `http::tests::serve_runs_the_mail_worker_on_its_own_connections_and_waits_for_it` polls `pg_stat_activity` (every 100 ms, under a 30 s `tokio::time::timeout` with a message naming what it waited for) until the mail worker's `LISTEN` connection appears, instead of asserting on one snapshot.

## Notes
- Issue #751. The single snapshot found 0 worker connections under full-suite load (seen in two PRs on 2026-11-19). The test has no other `pg_stat_activity` read: it does not check that the connections are gone after shutdown, so there was nothing else to poll. What it proves is unchanged: the worker's connections are named, one is a `LISTEN`, there are no more than `WORKER_POOL_SIZE`, requests are served on the one-connection pool, and `serve` returns Ok within 20 s of shutdown. (The test does not observe that the worker had stopped by then, so it does not guard `serve`'s await of the worker supervisors; that gap predates this change.)

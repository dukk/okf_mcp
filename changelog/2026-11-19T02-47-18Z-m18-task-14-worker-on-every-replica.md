# M18 task 14: every replica runs a worker (#692, closes #458)

## Added
- `mail::queue::WorkerPool`: the process's one pool for background workers, at most 3 connections (`WORKER_POOL_SIZE`, was 4), created and closed by `serve`.
- `mail::queue::prune_workers`: deletes `apalis.workers` rows last seen over ten orphan timeouts ago that no job names. Called from the hourly OAuth/mail prune, right after the mail job prune.
- `worker_shutdown_grace` (default `"30s"`): how long `serve` waits for the mail worker to finish the mail in hand after SIGTERM, then logs and returns.
- `http::serve_with` and `ServeOptions { worker }`; `ServeOptions::NO_WORKER` for tests that only enqueue.
- `Config::deprecation_warnings`, logged at `warn` by `serve_with`.

## Changed
- Heartbeat 10 s (was 30 s) and 6 missed heartbeats (apalis's default is 10): a dead replica's mail comes back after about 1 minute (`ORPHAN_TIMEOUT`), not 5.
- `serve` always runs the mail worker. `mail.worker` is now `Option<bool>`: it still parses, is ignored, and a warning names it.
- `run_worker` leaves its pool open; the pool's owner closes it.
- `docs/operations.md`: the mail worker's behaviour with several replicas, the connection count (3 per process), the failover time, the at-least-once guarantee and the grace period.

## Notes
- Delivery stays at least once: a replica killed after the provider accepted a mail and before the ack causes a second send.
- `prune_workers` keeps every row a job still references (`apalis.jobs.lock_by` is a foreign key with no `ON DELETE`), so the plan's `Queued`/`Running` condition was widened; see the plan's correction note under Task 14.
- The dead-worker test scales production timing (10 s heartbeat) to 1 s and bounds the wait at 60 s, so machine load cannot make it flaky.
- Task 15 must take `&WorkerPool` and not close it.

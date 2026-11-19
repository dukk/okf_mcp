# M18 Task 15: housekeeping as apalis jobs (issue #693, closes #356)

## Added

- `src/housekeeping.rs`: one apalis job type, `Housekeeping { task, slot }`,
  on queue `housekeeping`, with five tasks: `prune_changesets` (every 5
  minutes), `prune_oauth` (hourly), `prune_audit` (daily), `prune_workers`
  (hourly) and `cluster_stats` (every minute).
- Every serving replica runs a ticker (`tick_until`) that, once a minute,
  pushes the current slot of each task with idempotency key `"<task>:<slot>"`.
  A push first looks the key up and skips a slot that is already scheduled,
  so the repeats attempt no insert and leave no errors in the Postgres log.
  apalis-postgres's unique index on `(job_type, idempotency_key)` turns the
  rare insert that loses a race with another replica into an ignored unique
  violation, so each slot is scheduled once in the cluster.
- Every serving replica runs a housekeeping worker on the shared worker pool
  (Task 14). It polls every 10 seconds and holds no `LISTEN` connection, so
  the pool stays at 3 connections per replica.
- A failed task is put back with the mail queue's backoff (30 s, doubling)
  and tried at most 3 times in all; the next slot does the work again.
- Migration `0024_cluster_stats.sql`: a one-row `cluster_stats` table that
  the `cluster_stats` task writes (open changesets, mail queue depth by
  status). Each replica's ticker reads it into its gauges, so every replica
  reports the same numbers.
- Metrics `okf.housekeeping.runs{task,outcome}` (`okf_housekeeping_runs_total`)
  and `okf.housekeeping.duration{task}` (`okf_housekeeping_duration_seconds`),
  with a Housekeeping row on the example Grafana dashboard.
- `changesets::open_total`, `mail::queue::depth`.
- Test helpers `TestServer::start_shared_serving` and `sibling_serving`, for
  replicas that run the workers.

## Changed

- The poll pass no longer prunes or counts: the prune functions,
  `oauth_pruned`, `audit_pruned`, the per-catalog open changeset map and
  `App::open_changesets` are gone from `App`.
- `okf_changesets_open` has no `catalog` label: it is the cluster's total,
  present from the first scrape. The dashboard's panel reads
  `max(okf_changesets_open)`.
- `okf_mail_queue_depth` is now filled from the `cluster_stats` row, not by
  each replica's poll.
- `changesets::open_counts` is replaced by `changesets::open_total`, and
  `mail::queue::record_depth` by `mail::queue::depth`, which returns the
  counts.
- `http::serve_with` starts the housekeeping worker and ticker beside the
  mail worker (with `ServeOptions.worker`), supervises both workers with
  `worker_shutdown_grace`, and closes the worker pool after both stop.
- `prune_oauth` also deletes finished housekeeping jobs after a day and ones
  that gave up after 30 days.
- `docs/operations.md`: "Pruning" under multiple replicas now describes
  housekeeping, and the metric reference lists the new and changed families.

## Notes

- Delivery of a housekeeping job is at least once, like mail: a worker that
  dies mid-task loses the job to another after the orphan timeout. Every task
  is idempotent, so a second run does no harm.
- A slot missed because every replica was down is not caught up.
- Plan corrections: Task 15 has a dated "Corrected (issue #693)" note.

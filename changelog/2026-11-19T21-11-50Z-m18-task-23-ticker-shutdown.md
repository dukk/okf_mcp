# M18 Task 23: the housekeeping ticker stops promptly at shutdown (issue #748)

The ticker's shutdown branch raced only `ticks.tick()`. Once a tick began, a
query blocked on a lock (the slot push on `apalis.jobs`, or the
`cluster_stats` read) kept it running, and `serve_with` awaits the ticker with
no deadline, so the process could outlive `worker_shutdown_grace`.

## Changed

- `housekeeping::tick_until` runs each tick under
  `shutdown.run_until_cancelled`, as the change-feed listener does, and
  returns when shutdown wins. A dropped tick leaves nothing half done:
  scheduling is idempotent per slot and the gauge refresh only reads.
- Test `the_ticker_stops_at_shutdown_while_a_tick_is_blocked` holds ACCESS
  EXCLUSIVE on `apalis.jobs`, waits until the tick waits on the lock, cancels
  shutdown and asserts the ticker returns within 5 s.
- The ticker's await in `serve_with` now has no unbounded wait behind it; the other loops were not re-audited here.

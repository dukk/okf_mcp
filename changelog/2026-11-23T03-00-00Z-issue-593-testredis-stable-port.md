# Issue #593: TestRedis keeps its host port across start_again

## Changed
- `TestRedis::start` and `start_cluster` (`tests/support/redis.rs`) pick a
  free host port up front and start the container with it mapped
  (`with_mapped_port`). `start_again` therefore brings the server back on the
  same port, so `TestRedis::url` and every client built from it reach it
  again.
- The pick-then-bind has a race window with other test processes. `start_pinned`
  retries a failed start with a fresh port, up to five times.
- A cluster node loses `cluster-announce-ip` and `-port` on restart (`CONFIG
  SET` is not persisted); `start_again` sets them again and waits for
  `cluster_state:ok`. `start_again` does not add the slots again: it relies
  on `nodes.conf` staying in the container, and `cluster_state:ok` is what
  the wait checks. The first connect right after a restart was reset once
  by Docker's port forwarding while developing this, so that connect
  retries within a 30 s bound.
- `form_cluster` and `start_again` share `announce` and `wait_for_cluster`.
- The M16 plan has a dated correction where it expected a `start_fixed_port`.

## Added
- Tests `a_handle_built_before_a_restart_reaches_redis_afterwards` and
  `..._reaches_the_cluster_afterwards` (`tests/multi_replica.rs`): build a
  `Redis` handle, stop and restart the container, and check the URL is
  unchanged, the old handle answers `PING`, and (cluster) the node announces
  the URL's port. With `with_mapped_port` removed the standalone test fails at connect (the picked port is then not the one Docker maps).

## Decisions
- No restart-based resubscribe test here; the support now allows one.

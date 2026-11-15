# Fix: a daemon started by a gate no longer keeps the gate lock

## Fixed

- `tools/agentic/gate.sh` ran each locked gate as
  `flock -w <limit> <lock> <cmd>`, so every child inherited the lock's open
  file descriptor. The gate stops its own process group on exit, but a
  daemon that leaves the group with `setsid`, such as the sccache server
  `RUSTC_WRAPPER=sccache` builds start on demand, escaped that cleanup and
  held `gates.lock` for as long as it ran. Every later locked gate on the
  machine then waited until its lock-wait limit ran out (observed on
  2026-11-15: a lint and a test gate each queued about 25 minutes behind an
  idle sccache server).
- `gate.sh` now takes the lock itself on fd 9 (`flock -w`), before the
  gate starts, and holds it until `agentic_run_bounded` returns, so through
  the process-group cleanup too. It runs the gate with `9>&-`, so no child
  inherits the lock. Time spent waiting for the lock still counts against
  the gate's limit. A lock wait that runs out reports "waited its <limit>
  limit for the gate lock" instead of "was killed or timed out".
- A first version of this fix used `flock -o`. Review found that it freed
  the lock as soon as the gate's main command exited, while cleanup was
  still stopping TERM-ignoring leftovers (up to the 60 s grace period),
  so a second gate could run beside them.

## Tests

- `test_gate_daemon_that_escapes_the_group_does_not_keep_the_lock` in
  `tools/agentic/tests/run.sh`: a fake gate starts a `setsid` daemon and
  exits; the lock must be free while the daemon still runs. It fails on
  `main`.
- `test_gate_holds_the_lock_until_its_cleanup_ends`: while gate.sh is
  still stopping a TERM-ignoring leftover, `flock -n` on the lock must
  fail. It fails with `flock -o`.

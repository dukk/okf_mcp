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
- The gate now runs `flock -o`: flock holds the lock itself and closes the
  descriptor before running the command, so no child inherits it.

## Tests

- `test_gate_daemon_that_escapes_the_group_does_not_keep_the_lock` in
  `tools/agentic/tests/run.sh`: a fake gate starts a `setsid` daemon and
  exits; the lock must be free while the daemon still runs. It fails
  without `-o`.

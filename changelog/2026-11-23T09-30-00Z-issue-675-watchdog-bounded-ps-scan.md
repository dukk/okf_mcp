# Issue #675: watchdog.sh bounds its ps scans

## Problem

`tools/agentic/watchdog.sh` read the process table through three unbounded
`ps -eo ...` calls (the test and gate scan, the idle stdin reader scan and the
working-process scan of the quiet-agent check). A `ps` stuck on a `/proc` entry
would freeze the one tool meant to catch hangs, silently.

## Changed

- New `scan_ps <variable> <ps args...>` runs `ps` in its own session with its
  output in a private temp file (not a pipe), polls for it for
  `AGENTIC_CALL_TIMEOUT` seconds (default 60), and stores the output in a
  variable; the loops read that variable instead of a pipe from `ps`. On
  timeout it sends TERM, KILL after `agentic_kill_after` seconds, waits at
  most 2 s more, and then ABANDONS a child that is still there: it never
  `wait`s for it or for its output to close, deletes the temp file, and goes
  on (review of PR #947, round 2). `agentic_bounded_call` was not enough: its
  timeout branch ends in an unconditional `wait`, which never returns for a
  process in an uninterruptible /proc read, and `$(...)` also waits for the
  pipe to close.
- All three scans are taken before any alert is raised (review of PR #947): a
  scan that fails after an earlier one found something skips the pass too, so
  no alert survives it.
- A scan that times out or fails skips that pass with a warning on stderr (no
  scan output) and the next pass tries again. It is not an alarm; `--once`
  still exits 0.
- Tests: a stub `ps` that ignores TERM and leaves a detached grandchild holding
  its output open (the watchdog warns, skips and reaches its next pass, leaves
  no temp file; the pipe-based capture hangs on it). A process that survives
  KILL (a real D state) can't be made in a test, so the last branch (disown
  and go on) is not run directly; the test covers the TERM, KILL and
  grandchild-held-output paths. Also a stub `ps` that sleeps past a 1 s bound, in looping mode (two passes
  run, the watchdog keeps going) and with `--once` (exit 0).
- README and the script header describe the bound.

## Other unbounded calls

Surveyed for process-table commands (`ps`, `pgrep`) in `tools/agentic/*.sh`:
besides the three watchdog scans, only `pgrep -c -g` in `lib.sh` (line 56)
is unbounded. It counts what a finished command left in its own process group
and was not changed here. Other external commands the watchdog runs without a
bound (`readlink` and `getconf`) are local reads and were not part of this
survey.

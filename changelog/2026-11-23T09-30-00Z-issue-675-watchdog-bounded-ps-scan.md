# Issue #675: watchdog.sh bounds its ps scans

## Problem

`tools/agentic/watchdog.sh` read the process table through three unbounded
`ps -eo ...` calls (the test and gate scan, the idle stdin reader scan and the
working-process scan of the quiet-agent check). A `ps` stuck on a `/proc` entry
would freeze the one tool meant to catch hangs, silently.

## Changed

- New `scan_ps <variable> <ps args...>` runs `ps` through `agentic_bounded_call`
  (`AGENTIC_CALL_TIMEOUT`, default 60 s) and stores the output in a variable;
  the loops read that variable instead of a pipe from `ps`.
- All three scans are taken before any alert is raised (review of PR #947): a
  scan that fails after an earlier one found something skips the pass too, so
  no alert survives it.
- A scan that times out or fails skips that pass with a warning on stderr (no
  scan output) and the next pass tries again. It is not an alarm; `--once`
  still exits 0.
- Tests: a stub `ps` that sleeps past a 1 s bound, in looping mode (two passes
  run, the watchdog keeps going) and with `--once` (exit 0).
- README and the script header describe the bound.

## Other unbounded calls

Surveyed for process-table commands (`ps`, `pgrep`) in `tools/agentic/*.sh`:
besides the three watchdog scans, only `pgrep -c -g` in `lib.sh` (line 56)
is unbounded. It counts what a finished command left in its own process group
and was not changed here. Other external commands the watchdog runs without a
bound (`readlink` and `getconf`) are local reads and were not part of this
survey.

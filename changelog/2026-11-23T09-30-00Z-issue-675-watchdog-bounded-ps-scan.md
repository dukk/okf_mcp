# Issue #675: watchdog.sh bounds its ps scans

## Problem

`tools/agentic/watchdog.sh` read the process table through three unbounded
`ps -eo ...` calls (the test and gate scan, the idle stdin reader scan and the
working-process scan of the quiet-agent check). A `ps` stuck on a `/proc` entry
would freeze the one tool meant to catch hangs, silently.

## Changed

- New `bounded_capture [-q] [-i <file>] <variable> <cmd...>` in `watchdog.sh`
  (it replaces round 2's `scan_ps`, generalized, not copied). It runs the
  command in its own session with its output in a private temp file (not a
  pipe), stdin from `<file>` or `/dev/null`, polls it for
  `AGENTIC_CALL_TIMEOUT` seconds (default 60), and stores the output in a
  variable. On timeout it sends TERM, KILL after `agentic_kill_after`
  seconds, waits at most 2 s more, and then ABANDONS a child that is still
  there: it never `wait`s for it or for its output to close, deletes the temp
  file, and goes on. `agentic_bounded_call` was not enough: its timeout branch
  ends in an unconditional `wait` (`lib.sh:64-65`), which never returns for a
  process in an uninterruptible kernel wait, and `$(...)` also waits for the
  pipe to close. `lib.sh` is unchanged.
- Every external command a pass needs that reads the process table, another
  process's memory, git or the logs goes through it (review of PR #947, round
  3, which found the per-process `ps -p` re-reads still on
  `agentic_bounded_call`): the three `ps -eo` scans, both `ps -p` re-reads,
  the read of an idle `tail`'s `/proc/<pid>/cmdline` (`tr`; reading another
  process's command line reads its memory), `git worktree list`,
  `git reflog`, `find` over the logs, and the start-up `git rev-parse` (now
  read once instead of once per pass for the expectation ledger).
- What a timeout does: a timed-out or failed `ps -eo` scan skips the whole
  pass (all three are taken before any alert, so no earlier alert survives
  it); a timed-out re-read drops that process for the pass; a timed-out
  `find` or `reflog` skips that expectation for the pass; a timed-out
  `worktree list` leaves only the main checkout for the pass. Each warns on
  stderr, naming only the command. None is an alarm, the next pass tries
  again, and `--once` still exits 0. A failed or timed-out start-up
  `git rev-parse` exits 1 with an error.
- Tests: one stub per supervised call that ignores TERM and leaves a detached
  grandchild holding its output open (`ps -eo`, `ps -p` only, `tr`,
  `git worktree list`, `git reflog`, `find`): the looping watchdog warns
  twice, starts the hung command again on a later pass, raises no alarm and
  leaves no temp file; and the start-up `git rev-parse` exits 1. Each fails
  when its call site goes back to `$(agentic_bounded_call ...)` or `$(...)`,
  and the `ps -p` test fails against the previous `watchdog.sh` (6d33448). A
  process that survives KILL (a real D state) can't be made in a test, so the
  last branch (disown and go on) is not run directly; the tests cover the
  TERM, KILL and grandchild-held-output paths. Also a stub `ps` that sleeps
  past a 1 s bound, in looping mode (two passes run, the watchdog keeps going)
  and with `--once` (exit 0), and a stub that hangs only the third scan.
- README and the script header describe the bound and its remaining limit.

## Remaining limit

Not supervised: bash's own reads of `/proc/<pid>/comm`, `/proc/<pid>/stat`
and `/proc/uptime`, `readlink` of `/proc/<pid>/cwd` and `/proc/<pid>/fd/0`
(none of them reads the process's memory, which is what stalls `ps`), and
local commands (`dirname` and `getconf` at start-up, `mktemp`, `date`, `sleep`, `rm`, `sed` and `head`
on expectation files, and `grep`, `sed`, `awk`, `sort` and `tail` on text
already captured). A filesystem that hangs every access (the one holding
`$TMPDIR` or git's common directory) can still block those.

## Other unbounded calls

Surveyed for process-table commands (`ps`, `pgrep`) in `tools/agentic/*.sh`:
besides the watchdog's, which are all supervised now, only `pgrep -c -g` in
`lib.sh` (line 56) is unbounded. It counts what a finished command left in its
own process group and was not changed here.

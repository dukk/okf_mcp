## Added
- `tools/agentic/watchdog.sh` flags an idle stdin reader inside a checkout (issue #780): a `cat`, `tee`, `head` or `tail` without `-f` whose fd 0 is a pipe or socket, that has run longer than `AGENTIC_WATCHDOG_IDLE` (default 600 s) and used at most one second of CPU (`AGENTIC_WATCHDOG_IDLE_CPU`, in clock ticks). It uses the same plausibility checks as the other two (a reading above the host's uptime is ignored; a flagged process is re-read once).
- Tests in `tools/agentic/tests/run.sh` for it, including one that the default idle limit applies when the variable is unset.

## Changed
- Alerts no longer include command lines. Every alert, old and new, reports only pid, elapsed time, comm and cwd, because command lines can carry credentials. The checks still match on the command line internally.

## Notes
- A bash blocked in `read` is not checked: from /proc it looks like any bash that is idle for another reason, such as a shell waiting for commands on its stdin, so flagging it would raise false alarms.
- Why it matters: a background command's stdin never closes, so a stray `cat` hung for 2h20m on #430 with no output and no CPU, and the gate behind it never started.

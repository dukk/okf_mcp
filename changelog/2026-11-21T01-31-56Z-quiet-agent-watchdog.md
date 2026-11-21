# Quiet-agent detection (issue #850)

2026-11-21: a coordinator waiting for an agent's report had no way to notice that the agent had stopped for good; in M19 a green PR sat unreviewed for about three hours.

- `tools/agentic/expect.sh start|done|list`: a ledger of pending reports, one file per worktree basename under `<git-common-dir>/agentic-expect/`.
- `watchdog.sh`: exits 3 for a pending expectation whose worktree is idle. Idle means no cargo, rustc, just, flock, `gate.sh` or test-binary process with its cwd in the worktree (a gate waiting on the shared lock and a slow first build count as work) and no activity newer than `AGENTIC_WATCHDOG_QUIET` (default 1200 s). Activity is the newest of the mtimes under `target/i*-logs/`, the HEAD reflog time and the expectation's start. The alert names the worktree basename, label and idle seconds, no command line. A vanished worktree's entry is dropped with a note.
- `lib.sh`: `agentic_expect_dir`, `agentic_expect_field`.
- Docs: README tool table and coordinator loop, AGENTS.md tool table, a lessons.md entry.
- Tests: 13 new shell tests in `tools/agentic/tests/run.sh`. HEAD activity is the reflog event time (`git reflog --date=unix`), not the commit date.
- Choice: a restarted watchdog alerts again while the worktree is still idle; `expect.sh start` again restarts the quiet clock, `done` clears it.

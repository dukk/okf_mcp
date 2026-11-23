# Agentic development tools

These are the scripts and briefs used when a coordinator agent carries a
milestone's sub-issues through to merged PRs. Each sub-issue gets its own
background agent, git worktree and PR, and every PR is reviewed by the review
bot before it merges.

| File | Used by | What it does |
|---|---|---|
| `gate.sh` | implementation agents | Runs `fmt`, `lint`, `test`, `test-ui`, `dylint` (the policy lints, `docs/contributing/policy-lints.md`), `worker` (the reader's Node tests) or a narrow `cargo test`. Every gate has a hard timeout; the heavy gates also take a lock shared by all worktrees. Exit 124 means a test hung (a timeout, or a gate that was killed). Whatever the exit, `gate.sh` returns only once every process the gate started is gone and the lock is free. If a gate exits by itself but leaves processes behind, they are stopped, its own status is kept, and a warning says how many there were. |
| `watch-review.sh <pr>` | coordinator | Waits until the bot has reviewed the PR's current head (exit 0). Re-triggers the bot once if it has not reacted to this push within ~90 s, or has not reviewed within ~5 min; a reaction from an earlier round does not count. Gives up 15 min after it started, printing why (exit 2). Exits 4 at once when the round cap is reached instead of asking again. Exits 5 at once when the PR is `BEHIND` or `DIRTY` against `main`, or the bot answers with an "out of date" notice: merge `origin/main`, push and watch again. Every `gh` call is bounded by the time left, so a stalled call can't hold it past the deadline. |
| `merge-reviewed.sh <pr>` | coordinator | Merges only if the bot reviewed the current head (or the round cap is reached), every bot thread has a reply, and the PR is `CLEAN`. The merge is pinned to the inspected head with `--match-head-commit`. It then fetches `origin` and prints `MERGED PR <n> -> <base> <sha>`: the PR's real base branch and its merge commit (from `gh pr view --json mergeCommit`, else the tip of `origin/<base>`). |
| `watchdog.sh [--once]` | coordinator | Runs in the background for the whole epic. Exits 3 when any test or gate process in the main checkout or any registered worktree (wherever it lives) runs past its limit, or when an idle stdin reader there (a `cat`, `tee`, `head` or `tail` without `-f`, on a pipe or socket, with about no CPU use, older than the idle limit) is found, which catches agents stuck before they open a PR, including a background command hung on a stray `cat`. Alerts give only pid, elapsed time, comm and directory, never a command line, because command lines can carry credentials. A bash blocked in `read` is not checked: /proc can't tell it from a shell idling for another reason. A reading above the host's uptime (a wrapped `etimes`) is ignored, and a flagged process is read once more and must still exist, still be over its limit and still be plausible. Each `ps` scan is bounded by `AGENTIC_CALL_TIMEOUT` (output goes to a temp file, and a `ps` that survives KILL is abandoned, not waited for); a scan that hangs or fails skips that pass with a warning (never an alarm, and `--once` still exits 0), and the next pass tries again. `--once` checks a single time. |
| `expect.sh start <worktree> [label]`, `done <worktree>`, `list` | coordinator | Ledger of agent reports the coordinator is waiting for, one file per worktree basename under `<git-common-dir>/agentic-expect/`, shared by every worktree. `start` when you start an agent or send it back (a second `start` restarts the quiet clock), `done` when its report arrives. `watchdog.sh` then exits 3 when that worktree is idle: no cargo, rustc, just, flock, `gate.sh` or test-binary process has its cwd there (so a gate waiting for the shared gate lock, or a slow first build, counts as working), and the newest of the mtimes under its `target/i*-logs/`, its HEAD reflog time and the expectation's start is older than `AGENTIC_WATCHDOG_QUIET` (default 1200 s). The alert names the worktree basename, the label and the idle seconds, never a command line. An expectation whose worktree is gone is dropped with a note. A watchdog restarted after an alert alerts again only while the worktree is still idle. |
| `brief-common.md` | implementation agents | The shared rules every implementation agent follows. |
| `lessons.md` | implementation agents | Rules learned from real review findings. Extend it whenever a review finds a new class of bug. |
| `lib.sh` | the scripts | Repository, reviewer, review-round, paginated-API, clock and shared-state helpers. Also `agentic_run_bounded`, which every external command goes through: it runs the command in its own process group with a time limit, and always leaves that group empty. |
| `tests/run.sh` | anyone changing these tools | Shell tests: a stub `gh` driven by fixture files, a fake clock and throwaway repositories. Run `just test-agentic`. |

## Coordinator loop

1. **Start the watchdog** in the background, and restart it after each alert: `tools/agentic/watchdog.sh`.
2. **For each ready sub-issue**, create a worktree from a fresh `origin/main`:
   ```
   git worktree add .worktrees/okf-mcp-<N> -b <type>-issue-<N>-<slug> origin/main
   ```
3. **Start an agent with a short brief:** the issue, its worktree and branch, the plan task, and what runs in parallel. Point it at `tools/agentic/brief-common.md` and `tools/agentic/lessons.md`. Then record the wait: `tools/agentic/expect.sh start <worktree> "<issue and task>"`.
4. **When the agent reports a PR,** clear its wait with `tools/agentic/expect.sh done <worktree>` and run `tools/agentic/watch-review.sh <pr>` in the background.
   **Before any request for a (re-)review**, by hand or by the watcher, bring the PR up to date with `main`: `git merge origin/main`, push, and wait until GitHub no longer reports it `BEHIND`. The bot skips a PR that is out of date, so a request made before that wastes the request and the wait. `watch-review.sh` exits 5 instead of waiting while the PR is behind or conflicting with `main`.
5. **When the review lands,** send the same agent back with the comments, and re-arm the wait with `expect.sh start <worktree> "<label>"`. Repeat until a review leaves no new comments, or until the round cap (below) is reached.
6. **Merge** with `tools/agentic/merge-reviewed.sh <pr>`. Then remove the worktree and branch, and tell agents with open PRs to `git merge origin/main`.
7. **If an agent goes quiet** for longer than its gates should take, or the watchdog reports one with a report pending, check it directly: read the worktree's `target/i*-logs/`, its `git log` and the PR yourself, then act (message it, or finish the work). Don't wait for its report; an agent that says it waits on background work can have stopped for good. Use `ps` for its processes, and query `pg_stat_activity` in its test container. Don't wait for its report.

## Review rounds

A PR gets at most `AGENTIC_MAX_REVIEW_ROUNDS` (default 3) reviews from the bot, counted across every head. Asking again past that mostly produces nitpicks.

- In rounds 1 and 2, address every comment.
- In the final round, fix only correctness, security and data-loss findings. File everything else (style, extra tests, wording, refactors) as a follow-up issue labelled `review-followup`, and reply in each thread with the issue link.
- Then merge with `merge-reviewed.sh`, with no further review request. At the cap, `watch-review.sh` exits 4 instead of re-triggering, and `merge-reviewed.sh` accepts the latest review even if it is of an earlier head, still requiring a reply in every bot thread and a `CLEAN` PR.

## Testing these tools

`just test-agentic` runs `tests/run.sh` under a 2-minute timeout. It takes a few seconds, needs no network or Docker, and never touches GitHub or this checkout. Add a case there for every change to the scripts.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `AGENTIC_REPO` | from `gh repo view` | `owner/name` of the GitHub repository |
| `AGENTIC_REVIEWER` | `brooke-reviews[bot]` | Full login of the review bot, compared exactly (a trailing `[bot]` is optional on either side) |
| `AGENTIC_RETRIGGER` | `/prrb` | Comment that re-triggers the bot |
| `AGENTIC_WATCH_TIMEOUT` | 900 (seconds) | How long `watch-review.sh` waits in all before it exits 2 |
| `AGENTIC_CALL_TIMEOUT` | 60 (seconds) | Most any single `gh`, `git` or watchdog `ps` call may take. `watch-review.sh` also bounds each call by the time left before its deadline. A call that runs out of time is stopped with its whole process group |
| `AGENTIC_MAX_REVIEW_ROUNDS` | 3 | Most bot reviews a PR gets before it merges without another |
| `AGENTIC_TIMEOUT_<GATE>` | 5m / 30m / 75m / 60m / 45m / 5m / 15m | Per-gate limits for `gate.sh`: fmt, lint, test, test-ui, dylint, worker, narrow |
| `AGENTIC_GATE_KILL_AFTER` | 60 (seconds) | How long what is left of a gate's process group gets between TERM and KILL, after a timeout or once the gate exits. `gate.sh` returns only once the group is empty, so the lock is free |
| `AGENTIC_WATCHDOG_TEST`, `_GATE`, `_IDLE`, `_INTERVAL` | 1200, 4800, 600, 300 (seconds) | Watchdog limits and check interval |
| `AGENTIC_WATCHDOG_IDLE_CPU` | one second (clock ticks) | CPU an idle stdin reader may have used and still count as idle |
| `AGENTIC_NOW_CMD`, `AGENTIC_SLEEP_CMD` | `date +%s`, `sleep` | Clock and sleep for `watch-review.sh`; the tests replace them with a fake clock |

The gate lock is `.git/agentic/gates.lock` in the shared git directory, so every worktree uses the same one.

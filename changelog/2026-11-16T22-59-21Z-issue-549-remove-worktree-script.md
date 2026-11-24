# tools/agentic/remove-worktree.sh frees a worktree's Cargo build output (issue #549)

`git worktree remove` leaves a worktree's build output behind. With Cargo's `build-dir` set outside
the checkout, each worktree has its own hashed directory, and during epic #507 each agent worktree
left 30-47 GiB there.

- New `tools/agentic/remove-worktree.sh <worktree> [--branch] [--force-branch]`. It refuses the main
  checkout, unregistered paths and worktrees with a process inside, then runs `cargo clean` (bounded
  by `AGENTIC_CLEAN_TIMEOUT`, default 600 s; cargo resolves the build dir, nothing is hard-coded),
  then `git worktree remove`, then optionally `git branch -D` (only when merged into
  `AGENTIC_BASE_REF`, default `origin/main`, unless `--force-branch`). A failed or timed-out clean
  keeps the worktree.
- `lib.sh` gains `agentic_path_inside` and `agentic_procs_inside`; `watchdog.sh` now uses the former
  instead of its own copy.
- `merge-reviewed.sh` names the new script after a successful merge; the README table, coordinator
  loop step 6 and configuration table are updated.
- Nine new cases in `tests/run.sh` (stub `cargo`, throwaway repositories).

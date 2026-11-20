# merge-reviewed.sh names the PR's real base (#702)

## Added
- Two cases in `tools/agentic/tests/run.sh` with a milestone base: one with a merge commit from `gh pr view`, one falling back to the tip of `origin/<base>`.

## Changed
- `tools/agentic/merge-reviewed.sh` printed `MERGED PR <n> -> main <origin/main sha>` after every merge. It now prints `MERGED PR <n> -> <base> <sha>`, where `<base>` is the PR's `baseRefName` and `<sha>` is the merge commit (`gh pr view --json mergeCommit`), or the tip of `origin/<base>` after a fetch when GitHub does not return one.
- It refuses, before merging, when it cannot read the PR's base branch.

## Notes
- A failed fetch no longer hides the merge commit: the line is still printed, followed by a separate warning.

# merge-reviewed.sh names the PR's real base (#702)

## Added
- Five cases in `tools/agentic/tests/run.sh`: two with a milestone base (a merge commit from `gh pr view`, and the fallback to the tip of `origin/<base>`), and three for the failure paths (a missing or null `baseRefName` refuses; an unavailable origin keeps the SHA and warns; no findable merge commit warns without inventing a SHA).

## Changed
- `tools/agentic/merge-reviewed.sh` printed `MERGED PR <n> -> main <origin/main sha>` after every merge. It now prints `MERGED PR <n> -> <base> <sha>`, where `<base>` is the PR's `baseRefName` and `<sha>` is the merge commit (`gh pr view --json mergeCommit`), or the tip of `origin/<base>` after a fetch when GitHub does not return one.
- It refuses, before merging, when it cannot read the PR's base branch.

## Notes
- A failed fetch no longer hides the merge commit: the line is still printed, followed by a separate warning.

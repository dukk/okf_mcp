# remove-worktree.sh: ship it, and `--base` for milestone branches (issue #549)

The script from the 2026-11-16 entry was written but never merged. This change brings it up to date
with main and ships it.

- Merged `origin/main`. Main's newer `merge-reviewed.sh` (it names the PR's real base and merge
  commit), README rows (`expect.sh`, the watchdog's idle-reader check) and tests are kept; the
  script's own rows and its "next:" hint are added to them.
- New `--base <ref>` option. `--branch` deletes a branch only when it is merged into the base, which
  was always `$AGENTIC_BASE_REF` or `origin/main`. A sub-issue branch merged into a milestone branch
  isn't in `main` yet, so it was refused. Pass the milestone branch, for example
  `--base origin/milestone/<epic>-<slug>`. A `--base` with no ref is a usage error (exit 2).
- Two new cases in `tests/run.sh`: a branch merged only into the given base is refused without
  `--base` and deleted with it, and `--base` with no ref is refused.
- `just test-agentic` now allows 300 s instead of 120 s. The suite has 102 cases, and on a loaded
  host it took 141 s, so the old limit cut it off.

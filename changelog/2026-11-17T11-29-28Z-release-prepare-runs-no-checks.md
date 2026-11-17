# Release preparation runs no checks; shipping does

## Changed
- `just release-prepare` no longer runs `just fmt`, `just lint` and `just test` in its cargo-release hook. Preparation is local and publishes nothing, so a bad release commit there is undone with a reset. `just release-ship` still runs all three, on the same tree, before it pushes the branch, opens the PR or publishes the tag.
- Before this change, a release ran the whole suite twice on byte-identical trees: `ship` requires `HEAD` to be the prepared commit. That doubled the run time and the exposure to load-sensitive flakes (#500).
- `release.toml`'s comment and the operations guide's release section now say where the checks run. The guide also notes that these local checks are the only ones a release gets: the image workflows build and push without running tests.

## Fixed
- `scripts/test_release.py` had been failing since the guide's pinned versions moved past 0.9.0. Its fixture copied the real `docs/operations.md`, which pins the current version, while the fixture crate starts at 0.9.0, so the hook never found its four mentions. The fixture now rewrites the copied guide's pinned version to 0.9.0.
- The fixture also gains a stub `scripts/test-pg.sh`, since `just test` now runs through it, and the real script starts a Postgres container.
- All 32 tests pass.

## Notes
- `test_failed_prepare_checks_preserve_changes` is replaced by `test_prepare_runs_no_checks`. A check that would fail does not stop preparation, and it still stops shipping before any PR.
- Revert check: with `checks()` put back in the hook, the new test fails.

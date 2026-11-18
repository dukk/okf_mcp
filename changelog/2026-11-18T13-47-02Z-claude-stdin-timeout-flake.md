## Added
- `start_delay_ms` scenario key in the fake Claude child (`tests/fixtures/investigator/claude/fake_child.py`), which delays the fake's start-up so the load race can be reproduced deterministically.
- Test `investigator_claude_slow_start_is_retried_not_failed`, which starts the fake 500 ms late against a 200 ms deadline.

## Changed
- `investigator_claude_stdin_timeout_cancel_reap` no longer races the 200 ms run deadline against interpreter start-up. The timeout cases go through a new helper, `run_timed_after_start`, which retries on a fresh fake with a longer deadline when the child had not started before the deadline fired. The cancel case uses a 120 s deadline, so only the cancel can end it, and its wait is bounded by a 30 s timeout. The assertions are unchanged: a stalled stdin write times out as `Child`, a cancel gives `Cancelled`, and the child is reaped.
- `Fake::started` now waits for a complete `record.json`, not just an existing file.

## Notes
- Root cause (issue #307): `run_process`'s deadline covers spawn and the fake's interpreter start-up. Under load the 200 ms deadline expired before the fake wrote `record.json`. `Fake::started` then panicked with "fake did not start". The final case (`after_output_stall`) had no `started` wait at all, so `reaped` read a `record.json` that did not exist yet and failed with `NotFound`. Production code is unchanged.
- Checked with the old test body plus a 500 ms start delay: it fails with "fake did not start".
- `investigator_claude_timeout_includes_pipe_eof` has a 2 s deadline with the same shape (deadline covering start-up, then `reaped`). It is a possible follow-up and is unchanged here.

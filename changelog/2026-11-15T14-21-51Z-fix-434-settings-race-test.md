# Fix #434: the settings set/reset race test assumed FIFO lock wakeup

Issue #434 (duplicates #441 and #443).

## Added
- `store::settings::tests::a_set_queued_behind_a_new_override_records_it_as_from`:
  with no override stored, a held `server_state` row whose transaction
  also inserts an override of 3 queues a `set` of 4. Once released,
  `set`'s entry must say `from: 3`. It pins that `set` reads the previous
  value after the generation lock. A read taken before it, even
  `FOR UPDATE`, can neither see nor wait on a row an uncommitted
  transaction inserted, so it records `from: null`. Both mutants were
  checked: the existing `SELECT ... FOR UPDATE` moved before
  `bump_admin_generation`, and a plain `SELECT` moved there. Each fails
  the test with `{from: null, to: 4}`. The PR's first version used a held
  removal instead (review of PR #450). A locking read moved before the
  lock waits on the uncommitted delete and still records `from: null`, so
  that version could not tell the two orders apart.
- `tools/agentic/lessons.md`: Postgres does not wake row-lock waiters in
  the order they queued; assert a valid serial history instead.

## Changed
- `a_concurrent_set_and_reset_of_one_key_both_succeed_in_order` is now
  `..._in_either_order`. It still checks that both writes succeed within
  30 s after both were seen waiting in `pg_stat_activity`. It now reads every
  settings entry in id order and the stored override, and accepts exactly
  the two serial histories: set first (`changed {3 -> 4}`, `cleared {from: 4}`,
  no override left) or reset first (`cleared {from: 3}`,
  `changed {null -> 4}`, override 4 left).
- The helper `set_and_reset_queued_on_the_generation_row`'s doc no longer
  says the order is forced.
- The M14 plan's Task 6d note names the renamed test.

## Notes
- Cause: a test bug, not a `from` bug. `set` reads the previous value with
  a new statement after `bump_admin_generation` takes the `server_state`
  row, so under READ COMMITTED it sees whatever committed before it. The
  failing runs' `{from: null, to: 4}` can only come from a `reset` that
  committed first, because 3 was stored before either writer started.
  The two waiters race because the holder's `UPDATE` leaves a new row
  version, and each waiter retries against it once woken.
- Loops (`gate.sh narrow --lib`, one test per run): the old test failed
  1 of 100 runs alone, with the issue's `{from: null, to: 4}`. The new test
  passed 200 of 200 alone (one run took the reset-first order), 200 of 200
  as four parallel loops of 50, and 100 of 100 runs of every
  `store::settings::tests::a_*` test.
- Sibling tests: `many_racing_sets_and_resets_of_one_key_all_succeed`
  asserts nothing order-dependent. In the two admin-change tests the
  settings write waits on a row the promotion holds, not on the same
  holder, so their order is forced by a dependency, not by wakeup order.
  None needed changes.

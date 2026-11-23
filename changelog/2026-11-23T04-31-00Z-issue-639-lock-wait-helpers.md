# Issue #639: lock-wait test helpers read pg_locks, not stale query text

## Changed
- New test module `src/test_locks.rs` holds the lock-wait helpers. `Waiter`
  and `wait_waiter` moved there from `maintenance/final_tests.rs` unchanged;
  `wait_row_lock_waiter` and `expect_row_lock_waiter` are new.
- `wait_for_source_lock` (`maintenance/tests.rs`) and `blocked_in_lock_people`
  (`keys/audit_tests.rs`) no longer match `pg_stat_activity.query`, which
  shows the previous statement of a backend that was idle at snapshot time.
  They look for the row-lock wait itself.
- `blocked_in_lock_people` has its own 5 s timeout (`LOCK_PEOPLE_WAIT`),
  shorter than its callers' 10 s outer deadlines so its diagnostic fires
  first (review of PR #937); it panics with a message naming the `users` row
  it waited for and the holder. The wait is `lock_people_wait`, which takes
  the table and limit.

## Lock identity
Postgres reports a backend waiting for a row lock as two `pg_locks` rows
(checked by dumping `pg_locks` for a `FOR SHARE` waiting behind a `FOR UPDATE`):
a granted `tuple` lock (`RowShareLock`) on the table's relation, and an
ungranted `transactionid` lock (`ShareLock`) on the holder's xid. The
ungranted lock names no table; the granted `tuple` lock does. The helper
requires both, the waiter blocked by the holder's pid.
- maintenance takes the catalog's `documents` rows `FOR SHARE` -> `documents`.
- `lock_people` takes `users` rows `FOR KEY SHARE` -> `users`.

## Tests
- `test_locks::tests`: found by its table; no match for another table; no
  match behind a different holder.
- Checked the helpers can fail: pointing `wait_for_source_lock` at `catalogs`
  and `blocked_in_lock_people` at `teams` made all 7 tests that use them
  time out with their messages (then reverted).

- `lock_people_wait_matches_users_and_names_a_missing_wait`: a revoke queued
  in `lock_people` behind a held user row is found on `users`; a wait on
  `teams` returns the diagnostic (300 ms limit).

## Notes
- The table is the whole identity of a row wait. It does not tell the lock
  mode or the row apart, so another `FOR SHARE`/`FOR KEY SHARE` wait on the
  same table behind the same holder would match.

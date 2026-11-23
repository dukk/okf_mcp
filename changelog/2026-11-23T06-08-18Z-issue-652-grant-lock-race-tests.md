# Issue #652: tests for the OAuth grant lock protocol (#651 gaps)

## Added

- `oauth::token::tests::a_code_exchange_waiting_on_a_revocation_is_refused_and_mints_nothing`:
  a revocation holds the grant's `api_keys` row `FOR UPDATE`, the code
  exchange is confirmed waiting on that row (`test_locks::expect_row_lock_waiter`,
  behind the revocation's backend pid), then the revocation commits. The
  exchange answers `invalid_grant`, no `oauth_tokens` row exists for the
  grant, and the spent code cannot be tried again.
- `store::keys::idle_tests::a_user_deletion_queues_behind_an_admitted_refresh_and_removes_what_it_minted`:
  a refresh holds its grant, family and token rows, the user deletion is
  confirmed queued on the grant's `api_keys` row behind the refresh's pid,
  the refresh mints its successors and commits, and the deletion then
  cascades everything away. Neither side gets SQLSTATE 40P01.
- `store::keys::idle_tests::a_refresh_queues_behind_a_user_deletion_and_is_refused`:
  a deletion holds the grant (parked on a token row a third transaction
  holds, confirmed on `oauth_tokens`), the refresh is confirmed queued on
  the grant's `api_keys` row behind the deletion's pid, the parked row is
  released, and the refresh finds no grant (`RefreshUse::Unknown`).

## Notes

- Test-only; no production code changed.
- Revert checks: with `use_refresh` taking the grant after the token row
  (and with no grant lock at all, the pre-#651 order), the refresh/deletion
  tests fail with the database's deadlock abort (`Busy`, SQLSTATE 40P01).
  With the `lock_grant` check removed from the code exchange, the exchange
  answers 200 and mints tokens for the revoked grant.

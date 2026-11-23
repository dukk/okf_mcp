# Issue 589: revoke the grant when the final pending removal fails

`finish` (OAuth authorization) runs after the approval has created the grant,
by `code_rows::redeem` (sign-in code) or `grant_rows::create_self_issued`
(signed-in consent). When the last step, removing the pending authorization
from Redis, failed, the request answered the 503 busy page and minted no code,
but the grant it had just created stayed live in the user's grant list.

## Added

- `revoke_unused_grant` in `src/oauth/authorize.rs`: revokes one grant by its
  id (`keys::revoke_by_id`, system actor `oauth`), best effort. A failure is
  logged with `split_event!` and does not change the answer.

## Changed

- `finish` calls it when `oauth.pending.remove` fails, before answering the
  busy page. The answer, the absence of a code and the success path are
  unchanged.
- `a_failed_removal_answers_the_busy_page` first completes an earlier consent,
  then asserts that after the refused approval the live OAuth grants are
  exactly the earlier one.

## Notes

- Why undo instead of reorder: the grant exists before `finish` runs, and in
  the sign-in-code path the code is spent by the same transaction that creates
  the grant. Removing the pending request earlier would also drop it on a
  wrong code, where it must stay for another try. Undo needs no change to
  that order.
- Pre-existing grants are safe: both ways of creating a grant insert a new
  row, and the cleanup revokes only that row's id.
- Revoked, not deleted: the revocation keeps the grant's audit trail.
- Not changed: when the authorization code cannot be stored (a full map or a
  Redis error) the grant also stays; see the issue's follow-up candidate.

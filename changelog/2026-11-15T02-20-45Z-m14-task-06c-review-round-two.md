# M14 task 06c: round-two review fixes

## Added

- `owner_deleted_while_revoke_locks_answers_unknown_key` and
  `owner_deleted_while_assign_locks_answers_not_found`: hold the admin actor's
  row, wait until the change is blocked in `lock_people`, delete the key's
  owner (which deletes the key), release, and check the caller's usual
  unknown answer (`no active key` for a revoke, `NotFound keys/<id>` for an
  assignment). All waits are bounded.
- `grant_and_ungrant_by_id_name_an_oauth_grant_as_a_grant`: granting and
  ungranting a catalog on an OAuth grant by id records target kind `grant`.
- `keys::key_target`, the one place that picks `Target::Key` or
  `Target::Grant` (kind `oauth`) for key and grant entries.

## Changed

- `audit::lock_credential_people` returns before its snapshot comparison
  when the credential is gone under its lock, so a credential deleted with
  its owner meanwhile gets the caller's unknown-target error instead of
  "credential changed while locking; retry the request".
- `grant_by_id`/`ungrant_by_id` (and their name-resolving twins) record an
  OAuth grant as a `grant` target, as revoke and assignment already did.
- `record_change` takes the action as a closure over the target, so revoke
  reads `kind` once; `set_user` uses `key_target` instead of its own copy.
- The Task 6c plan table and `docs/operations.md` say grant entries may
  target a `grant`, with a dated correction note in the plan.
- The race tests share one `blocked_in_lock_people` wait helper.

## Notes

- Addresses PR #424 inline review comments 4573523050, 4573523055 and
  4573523059.
- Revert checks: without the early return both owner-deletion tests fail
  with the retry refusal; with `Target::Key` restored in grant/ungrant the
  OAuth test fails on target kind; with revoke's action fixed to
  `key.revoked`, the existing codes/grants audit test fails, so the
  refactor's action choice is covered. Logs: `target/i398-logs/r2/`.

# Unlocks are created only by an admin user's credential (issue #447)

The maintainer decided on 2026-11-15 that the person behind the
credential that creates an unlock must be an admin user (spec Part 6,
"Made by an admin user"), so the audit log's granting person is always
operator staff. Raised in review of PR #437.

## Added
- `whoami`'s `user_admin`: whether the credential's user is an admin user
  (`false` without a user, or once the user is deleted).
- `unlocks::require_staff`: `unlocks::create` refuses an actor whose user
  is not an admin user with `OkfError::Forbidden` (403), read after
  `audit::lock_people` has the actor's row `FOR KEY SHARE`, so a demotion
  committed while the unlock waited is seen. The CLI's actor has no user
  and passes.
- Tests: `a_non_admin_users_every_catalog_admin_key_cannot_unlock_and_writes_nothing`
  (naming an admin, naming nobody and naming an unknown user: all 403, no
  unlock row and no `catalog.unlocked` entry),
  `a_non_admin_users_key_may_still_end_an_unlock_of_a_catalog_it_administers`,
  `only_an_admin_user_or_the_command_line_creates_an_unlock`,
  `an_unlock_waiting_on_its_creators_demotion_is_refused` (a real
  `users::set_admin` paused at its generation bump while the unlock queues
  on the users' rows), `whoami_says_whether_the_keys_user_is_an_admin_user`,
  and the journey `a_non_admin_users_admin_key_is_not_offered_unlock`.

## Changed
- `POST /api/v1/unlocks` refuses a credential whose user is not an admin
  user with 403 before the target is chosen, from the key cache; the store
  checks again under the lock.
- The console's `canUnlock()` also needs `whoami.user_admin`, so Browse's
  Unlock, the toast's Unlock and the Unlocks page's "Unlock a catalog" are
  offered only to operator staff.
- `the_hours_default_to_the_limit_and_an_admin_may_name_another_admin` is
  now `..._and_an_admin_user_may_name_another_admin`: it names `staff` with
  admin user `ops`'s every-catalog key, not a non-admin user's. The
  scoped-admin case of `unlocks_refuse_everyone_but_admins_and_scope_a_scoped_admin`
  uses a scoped key of admin user `staff`, so it still tests
  `require_every_catalog` rather than the new refusal.
- Docs: the spec's Part 6 and "Decided after the milestone" (replacing the
  open question), the M14 milestone changelog's open question, the plan's
  Task 9d and 9e `Corrected (issue #447)` notes, and `docs/operations.md`'s
  Unlocks API and `whoami` row.

## Notes
- **Ending is unchanged.** `DELETE /api/v1/unlocks/{id}` needs an admin
  credential that administers the catalog, not an admin user. Ending only
  takes access away; refusing it would only make it harder to close a
  customer's catalog (for example with the every-catalog key of an
  operator account that is not an admin user, or after the unlock's own
  admin was demoted). The spec's "ended by its admin or any admin" reads
  as any admin credential.
- **The CLI is unchanged**: `okf-mcp unlock create` acts on the database
  as the operator, with no user.

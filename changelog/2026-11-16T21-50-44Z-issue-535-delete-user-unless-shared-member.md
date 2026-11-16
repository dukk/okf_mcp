# An optional "unless a member of a shared team" precondition on user deletion (issue #535)

## Added
- `DELETE /api/v1/users/{name}?unless_shared_member=true`: deletes nothing and answers 409 while the user is a member, in any role, of any team but its own personal team (suspended teams included). The message starts `the user is a member of shared team(s): ` (`users::SHARED_MEMBER_REFUSAL`); every other deletion refusal starts `user "<name>" cannot be deleted`.
- `users::DeleteOptions { allow_last_admin, unless_shared_member }` replaces the `allow_last_admin` bool of `users::delete`; `DeleteOptions::OPERATOR` is the CLI's.
- Tests: store (plain member, no shared team, suspended team and sole owner, a membership committed just before the check), REST (409, unchanged behaviour with `false` or no parameter, a misspelled parameter is 400, no shared team is 204) and a provisioner key.

## Changed
- The shared-team lookup in `users::delete` now returns the team names, still under `FOR UPDATE`.

## Notes
- The check is atomic: a membership insert needs the user row `FOR KEY SHARE`, which the delete's `FOR UPDATE` on the user holds off, so a membership either commits first (and is seen, so 409) or waits and then fails its foreign key.
- The refusal is checked before the owned-catalog, last-owner and last-admin refusals, so it wins.
- `docs/operations.md` documents the parameter and what a client matches on.

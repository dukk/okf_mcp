# Team owners manage member access and entitlements (issue #352)

Time: see the file name (UTC).

## Why

- Only admins could set a catalog's team member access or entitle a user, so every change
  to who reaches a shared catalog went through an admin, though the team's owners know
  who should have it.
- pgrace's decision on the issue (2026-11-20) fixed the shape: console session only,
  members of the owning team only, shared teams only, one identical refusal for an
  unknown user and one outside the team, the catalog drawer as the surface.

## What changed

- `store::ceiling`: `change_member_access` (with `MemberAccessChange.owner`),
  `entitle_as_owner`, `unentitle_as_owner` and the read `team_access`. The owner is
  checked in the write's transaction under its membership row lock
  (`catalogs::lock_shared_team_owner`, which refuses a personal team), then, for an
  entitlement, the target's membership row, then the catalog row. Lock order is the
  documented one (users, teams, catalogs, `server_state`). Ceiling writes still bump
  `admin_generation` and send the same invalidations; entries are audited with the owner
  as actor. `set_member_access`, `entitle` and `unentitle` (admins, the CLI) are unchanged.
- API: `PUT|GET /catalogs/{c}/member-access` and `PUT|DELETE /users/{u}/entitlements/{c}`
  accept an owner's console session (`api::owner_session`); keys and OAuth grants keep
  the admin-only refusal. New `GET /catalogs/{c}/entitlements` (the owning team's members
  and the catalog's entitlements, an owner sees its members' only), paged with keyset cursors
  (`members_after`, `entitlements_after`, `limit`, `list`); the owner's `GET member-access`
  reads the scalar only (`member_access_as_owner`). An owner's entitlement
  answer holds only the entitlement on that catalog; an admin's still holds all of them.
- Console: the owner drawer of a shared team shows Team member access and Entitlements
  (`pages/catalogs.js`), as pending writes keyed by catalog and user, with Load more
  for the members and the entitlements.
- Docs: `docs/operations.md` ("The ceiling and member access", the API reference and the
  parity table) and the user-login spec's Future work item 1.

## Notes

- `may_set_member_access` is unchanged: it stays the check for keys and the CLI.
- An entitlement an admin gave to someone who has since left the team is not shown to the
  owner and cannot be removed by one; an admin still can.
- A suspended owner team is not refused here, as the maintenance switch is not.

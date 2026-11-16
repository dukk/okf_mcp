# Issue #502: a provisioner key may list teams

## Added
- `("GET", "/api/v1/teams")` in `PROVISIONER_ROUTES` (`src/http/api/mod.rs`), so an external provisioning system can reconcile its records against the server's teams with its provisioner key instead of a second, admin credential.
- Tests (`src/http/api/provisioner_tests.rs`): a provisioner gets 200 and the same `{teams}` an every-catalog admin gets (shared, suspended and an admin user's personal team included); the neighbouring reads it could not make before (`GET /api/v1/users` without `email`, `/unassigned`, `/catalogs`, `/keys`, `/audit/export`) stay 403; `HEAD /api/v1/teams` stays 403.

## Changed
- `list_teams` (`src/http/api/people.rs`) checks `require_reader` instead of `require_admin`: a provisioner or any admin, as `GET /api/v1/teams/{name}` already did. Read, write and non-admin keys still get 403 (the existing access matrix in `people.rs` covers them).
- The allow-list test, the module docs, `docs/operations.md` (allow-list table, now 17 pairs, and the API reference's Key column) and the operator API design's Part 4 list the route.

## Notes
- The answer is the existing `TeamList` of `TeamView`s: names, members, owned catalog names and suspension state. It carries no catalog content, which a provisioner still cannot reach.
- Auditing: the provisioner's other reads (`GET /users?email=`, `GET /users/{name}`, `GET /teams/{name}`) record no use-log entry; #423 tracks that gap. This read follows them and records none either.
- Members of a team include admin users, as `GET /teams/{name}` already showed a provisioner; the user list's admin filter is for user lookup, not team membership.

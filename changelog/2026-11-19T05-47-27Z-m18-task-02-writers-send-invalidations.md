# M18 Task 2: every administrative write and publish sends its invalidations

Issue #680, plan `docs/superpowers/plans/2026-11-18-okf-mcp-m18-bounded-memory.md` Task 2,
spec Decision 10.

## Added

- Every store function that bumped `admin_generation` now calls
  `invalidate(conn, &[..])` in the same place in its transaction, naming what it changed:
  `catalog_meta` for catalog create, delete, details, owner, maintenance and member access
  (and for each catalog of a team being suspended or unsuspended); `team` for team writes,
  for a catalog's old and new owner, and for the teams of a deleted user; `user` for user
  writes, entitlements, unlocks and memberships; `credential` for every key and grant
  write, OAuth code redemption and self-issued grant; `settings` for settings writes.
- Publish, import and repair send `catalog { id, generation }` for the generation they
  committed.
- `Invalidation::credential`, `user` and `team` constructors.
- `invalidate::testing::invalidations_of`: listens, runs a write, and returns what it
  committed, ending on a marker it sends afterwards (commit order) rather than a timeout.
- `src/store/invalidation_tests.rs`: one test per writer, including no-op and refused
  writes that send nothing; a publish test in `mcp::changesets::publish_tests`.
- Two-replica tests: a store import served on both replicas, a store revocation refused on
  the other replica, and a revocation after the listener connections are killed.

## Changed

- `bump_admin_generation` moved from `src/store/catalogs.rs` into `src/invalidate.rs` as a
  private function: only `invalidate` can bump, so no change can bump without telling the
  other replicas.
- A list of only `catalog` invalidations does not bump `admin_generation`: publishes never
  did, and a bump would serialize every publish on the `server_state` row and reload every
  replica's key cache after each publish.
- `unlocks::end` of an unlock whose user was deleted invalidates nothing (and so no longer
  bumps): it reaches nobody.

## Notes

- Nothing is sent on rollback: each `NOTIFY` runs inside the writer's transaction. Each
  `invalidate` call replaced its `bump_admin_generation` in place, so every writer's lock
  order (users, teams, catalogs and changesets, `server_state`, the rest) is unchanged; the
  only new reads (a user's memberships in `users::delete`, a team's catalogs in
  `teams::suspend`/`unsuspend`) take no lock, and the owner ids come from existing
  `RETURNING` clauses.
- The command line uses the same store functions, so its changes reach running servers
  with no Redis announcement and no poll.
- Plan corrections are in Task 2 of the plan (dated `Corrected (issue #680)` notes).

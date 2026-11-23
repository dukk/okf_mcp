# M21 Task 10b (part 1): hide unprovisioned catalogs from every by-name reader

Part of issue #969. Review of the two-step create (PR #999) showed that the readers Task 8 left unfiltered would let a release-2 replica see or act on a half-made catalog the moment 10b writes one. This part ships the readers first (decision P11); nothing writes an unprovisioned row yet.

## Changed
- `provisioned_at IS NOT NULL` is added to `catalogs::find`, `catalogs::list`, and the by-name subqueries of `set_owner`, `update_details` and `set_maintenance`; to every `ceiling.rs` read and by-name write (`highest`, `page`, `ceiling_in`, `entitlements_in`, `member_access`, `team_access`, `entitle`, `set_member_access` and the locks beneath them); to the team catalog list (`teams.rs`), `keys::catalog_ids` and `repair::missing_root_index`.
- Plan: P11, the release graph and Tasks 8 and 10b say these filters end release 2.

## Not changed
- Readers that take a catalog id only (unlocks, sign-in codes, key and changeset joins, audit rendering, labels, export) and the deletion counts for teams and users: an id is only ever handed out for a provisioned catalog, and counting a half-made catalog toward a deletion refusal is the safe direction.

## Tests
- `src/store/catalogs_unprovisioned_tests.rs` seeds a row with `provisioned_at` NULL directly and checks that each reader above hides it (and shows it once provisioned).
- No mixed-version harness exists; the release order enforces the protocol.

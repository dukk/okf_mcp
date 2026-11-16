# Publish and user deletion take users in one order (issue #418)

## Fixed
- A publish by a user's key that was running when `users::delete` started on that user could deadlock with it: the publish held its changeset and then locked the user `FOR KEY SHARE` in `audit::record`, while the deletion held the user and then waited for the changeset. Postgres aborted one side as `Busy`. The publish now locks the users its `catalog.published` entry resolves (the actor, its key's owner, a personal owner team's person) right after the in-process publish mutex and before the advisory lock and the changeset, the order `users::delete` takes them in.
- The same inversion in the two other changeset writers that record an entry under the changeset lock: an admin's `discard` of another key's changeset (whose admin actor `users::delete` locks first, with every admin) and an audited edit (`revert`, through `write::apply_audited`).
- `users::delete` locked its actor and the people its discard entries resolve only at the end, in `audit::record`. It now locks the user through `audit::lock_people` (`target: (name, RowLock::Update)`, with the catalogs of the user's open changesets, read without a lock), after every admin row, so they are taken in `set_admin`'s order with the user.

## Added
- `store::publish::lock_people(conn, actor, catalog)`: `audit::lock_people` with one catalog, shared by publish, discard and the audited edit.
- Tests in `mcp::changesets::lock_order_tests`: three forced interleavings (publish, admin discard and revert, each held after its changeset lock while the deletion queues) and a repeated unforced publish/deletion race (20 rounds). All four failed with `Busy` before the fix.

## Notes
- The users are locked after the in-process mutex, not before it: a publish waiting on the mutex while holding a user row could form a cycle through the mutex that Postgres cannot detect, and would hang instead of failing.
- The M14 plan's Task 4 note and the M14 design's gaps list are corrected.

# Issue #423: audit a provisioner's email lookups and refusals

## Added
- Audit actions `user.looked_up` and `provisioner.refused` (`src/store/audit.rs`), and target kind `route` (migration `0031_provisioner_audit.sql`, which widens the `target_kind` check `NOT VALID`: the new set is a superset, so no scan of the log).
- `audit::record_provisioner_use`: one entry per key, event kind and clock hour with an exact count in `audit_access_counts` (as `catalog.admin_read` does), at most `PROVISIONER_ENTRIES_PER_HOUR` (50) entries per key per hour with the rest counted in one overflow entry per action. The entry belongs to no team, so owners never see it.
- `App::note_provisioner_use`: the background write, limited to `MAX_PENDING_PROVISIONER_WRITES` (32) in flight; a request that finds them all busy loses its count.
- `provisioner_gate` (`src/http/api/mod.rs`) now records every refusal of a provisioner, its own (`route_not_allowed`) and a handler's or store check's 403 (`forbidden`), with the method (standard methods only, else `OTHER`) and the route template. `list_users` records an email lookup's outcome and matched user.
- `AdmittedActor` (the identities fixed at admission) shared by `AccessRecord` and the new `ProvisionerUse`; `Principal::admitted_actor`; `Requester::of_extensions`; `UserRecord.id`.
- Console Audit page: the request count shows for the two new actions.
- Tests: `src/store/audit.rs` (collapse under concurrency, the hourly cap and its overflow, found and not-found lookups, pruning the budget counters) and `src/http/api/provisioner_tests.rs` (found and not-found lookup with no address stored, an admin match recorded as not found, a refused route by template with no literal path or query, a burst collapsing, a state change still writing exactly one entry and reads and no-ops none, owners seeing neither action).

## Changed
- `audit::prune` also deletes the budget counter rows of past hours (rows with no entry to cascade from).
- Operator API design Part 4 and its "What shipped differently" note, the M14 plan's pointer to #423, and `docs/operations.md` (provisioner paragraph, audit table, a paragraph on the bounds) say what ships: every state change, every email lookup and every refused request is audited with the key as actor; other reads and no-ops appear in request traces and metrics.

## Notes
- Decision (maintainer, issue #423, 2026-11-20): option 1 in part. Chosen here: no hash of the address. A found lookup names its user and a not-found one has no user, so nothing needs to correlate lookups of one address; a keyed hash would only add a linkable value and a key to rotate.
- Visibility: admins see the entries, a provisioner key sees the ones it wrote (as it sees its other entries), team owners see none (no team, operator actor).
- An admin user is never shown to a provisioner, so a lookup that matches one is recorded `not_found`, as the key saw it.
- Counts are exact except under overload: past 32 pending writes a replica drops the overflow's counts (debug event). The same holds for a failed write, which is logged.
- Not done: finer refusal codes than `route_not_allowed` and `forbidden` (a handler's reason text is free text, so the code stays coarse).

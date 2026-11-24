## Added
- `OpOutcome::rebased`: touched paths whose entry starts again on the revision published now, dropping the earlier entry's base and replay. Only `revert` sets it.

## Changed
- `revert` of a path the changeset had already edited bases the entry on the revision published now, as it already did for a path with no entry (issue #788). Before, the entry kept its first edit's base and lost its replay, so a revert to the published revision (the natural way to catch up a stale entry) left an entry that was stale, unreplayable and could never publish until the path was discarded. Now a `log` after such a revert is a clean one-line change on that revision, `pending_changes` shows it with `stale: false`, and publish writes it.
- A revert to an older revision, or to a `delete` revision, takes the same base: publish writes its text (or the deletion) over the revisions published since, and conflicts only if the path is published again after the revert. A revert to a deletion of a path deleted since leaves no entry.
- The `revert` tool description says what its base is.

## Notes
- The base is the path's revision in the in-memory snapshot the write's overlay is built on. A snapshot is only ever swapped in from committed state, so it never runs ahead of the database, so a publish that commits after it (or that this replica has not loaded yet) leaves the base behind the published revision, and publish reports a Conflict rather than writing over it.
- A revert is a free edit: it has no replay, and edits after it (a `log`) do not make the entry replayable. A publish of the path after the revert therefore conflicts, and the changeset has to revert again. Replaying a whole-file choice onto newer text would silently drop the revert.
- A revert of a path with no entry behaves as before.

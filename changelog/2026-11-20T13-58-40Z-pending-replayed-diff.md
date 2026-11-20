## Added
- `pending_changes` file records gain `replayed` (the diff shows the entry replayed onto the published text) and `replay_error` (why publish cannot replay a stale, replayable entry; null otherwise).

## Changed
- `pending_changes` shows a stale, replayable entry (a `log` entry or an `index` listing made before another changeset published the same file) as publish would write it: its operations redone on the published text, with `replayed: true` (issue #787). Before, it diffed the entry's stored text, built on the old base, against the published text, so every line published since showed as a deletion even though publish keeps them.
- A stale, replayable entry whose replay would fail at publish (a `SetListingSummary` whose bullet is gone, or a replay publish refuses with Conflict) gets `replay_error` and a one-line note in `diff`, in place of a diff of the stored text.
- `store::publish::replayed_text` takes the live published bytes instead of publish's private row type, and is `pub(crate)`: publish and `pending_changes` call the same function, so what `pending_changes` shows cannot drift from what publish writes.

## Notes
- Entries that are not stale, and stale entries with no replay (free edits, which publish refuses with Conflict), keep the diff of their stored text.
- `read_document` and `get_concept` through a changeset still show a stale entry's stored text. Replaying there means changing the changeset overlay, which writes build on too, so it is left for a follow-up.
- `revert` onto a path already in the changeset keeping the old base is issue #788, not fixed here.

# Frontmatter bookkeeping tools replay at publish (issue #830)

Time: see the file name (UTC).

## Why

- `stamp`, `review`, `question`, `unquestion`, `add_source` and `remove_source`
  each make a small edit of one frontmatter key, but stored it as a whole-file
  write. Any publish of the concept while the changeset was open left the entry
  stale and not replayable, and publish refused it with Conflict. The agent had
  to discard the changeset and make the changes again.

## What changed

- New `ReplayOp` variants: `AddSource`, `RemoveSource`, `Stamp`, `SetReviewed`,
  `AddQuestion` and `RemoveQuestion`. Each tool records its variant(s); `review`
  records `SetReviewed` plus an `AddQuestion` per question it queued.
- `Stamp` and `SetReviewed` keep the actor and time of the original call, so a
  replayed stamp reads as made when the agent made it.
- `frontmatter::replay_edit` redoes an op on the current text through the same
  edit functions the tools use (`add_source_edit`, `remove_source_edit`,
  `stamp_edit`, `set_reviewed_edit`, `add_question_edit`, `remove_question_edit`),
  so an edit and its replay cannot drift apart. The file's line endings are kept.
- A replay that can no longer apply is refused with the tool's own reason, which
  `pending_changes` shows as `replay_error` and publish returns: a source id the
  text now has, a source that is gone or now cited by a footnote, a question to
  remove that is no longer queued, `sources` or `open_questions` no longer a
  list. Queuing a question that is already queued changes nothing, as `question`
  does.
- `RemoveQuestion` matches the question's exact text and never reads it as an
  index. An `unquestion` of a question that is not a string records no replay (a
  free edit, as before).
- A free edit (`edit_document`, `set_frontmatter` ...) of the same path in the
  changeset still clears the replay, so publish conflicts as before.
- `StampField` now derives `Serialize`. The tool descriptions and
  `docs/reference/tools.md` say these edits are replayed.

## Tests

- Unit (`okf::ops::frontmatter`): each op's replay on text with a newer key
  equals the op's own result; the stamp keeps its original time and actor; the
  refusals; CRLF; an all-digit question text is not an index; what each tool
  records.
- Integration (`mcp::writes::frontmatter_replay_tests`, Postgres): `add_source`,
  `remove_source`, `question` with `unquestion`, and `stamp` with `review`,
  each replay over a concurrent publish (shown by `pending_changes`, published
  with the path in `replayed`); the same source id on both sides, a source now
  cited, and a free edit on the path.

## Notes

- Not done: `pending_changes` still diffs a stale, non-replayable entry's stored
  text, without a note that publish will refuse it with Conflict (the issue's
  optional item).

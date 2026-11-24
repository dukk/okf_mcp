## Added
- `ReplayOp::Unlink { target }` (`{"op": "unlink", "target": ...}`): turns every link of a `log.md` into a deleted concept into its link text (`[X](/infra/x.md "t")` becomes `X`), keeping every other byte, `\r\n` endings included. Publish replays it onto newer text, so a `log` entry another changeset publishes while a delete is pending lands in either order without a conflict. A stored op whose target is not a concept is refused, as a `Relink` is.
- `relink::links_to` and `relink::unlink_text` (crate-internal): the one test of "a link into a document" that the delete's `StillLinked` check and its unlinking share, resolved as a move's link rewrite resolves it (relative and absolute paths, `./` and `..`, `#fragment`, a title, percent-encoded paths).
- Tests: unit tests for the op, its replay (idempotent, both orders against a `log` entry, a link whose text forms a new link) and the delete (every link form, a `log.md` in a folder, the refusal still listing a concept and another `index.md`, and parity with `move_concept`); end-to-end tests of issue #658's repro, a later `log` entry, a concurrent `log` entry published before and after the delete, and the refusal for links outside `log.md`.

## Changed
- `delete_concept` no longer refuses with `StillLinked` for links from a `log.md` (issue #658). Only the `log` tool writes `log.md` and it only appends, so a concept a log entry linked could never be deleted. The delete now turns those links into plain text, records an `Unlink` for each `log.md` it changed, and adds a note per file (`note: /log.md: 1 link to /infra/x.md became plain text`). Links from concepts and from other `index.md` files still refuse.
- The `delete_concept` tool description (and `docs/reference/tools.md`), the README's tool table, the design's tool, replay and error rows, and the M6 plan's Decision 4 (a dated `Corrected` note) say so.

## Notes
- `move_concept` already rewrote links in `log.md` (`rewrite_links` plus a replayable `Relink`); the delete resolves links the same way, and a unit test checks that it unlinks exactly the links a move redirects, line for line.
- A link without `.md` (`[x](/infra/x)`) is not a link into `infra/x.md` for the link graph, the refusal or the move, so the delete leaves it too.
- Revert-checked: with `unlink_text` made a no-op, the end-to-end repro and both concurrent-order tests fail with `LintFailed` (the publish's link into the deleted path), and five unit tests fail.

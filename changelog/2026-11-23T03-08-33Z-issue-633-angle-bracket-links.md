# Issue #633: angle-bracket link destinations

## Changed
- `LINK` (`src/okf/markdown.rs`) also matches a CommonMark angle-bracket
  destination, `[x](<a b.md>)` and `[x](<a b.md> "title")`. The destination may
  hold spaces and `)`; it may not hold a line break, `<`, `>` or the
  `\x1c-\x1f` control bytes (the same bytes the plain form excludes), and
  `[x](<>)` is not a link. `[x](<a)` is still the plain destination `<a`. The
  new alternative is one more branch of a linear-time `regex` pattern, so
  nothing can backtrack; a test feeds it a 400 KB unclosed `(<`.
- `LinkMatch::target` and `target_range` exclude the brackets. Every consumer
  that goes through `find_links` (snapshot link graph and backlinks, lint,
  `links_report`, `move_concept`, `relink`, delete, index replay) now sees
  such links; the console renderer already used pulldown-cmark and did.
- `relinked_bullet` (`moves.rs`) replaced `(<target>` textually; it now
  replaces `target_range`, as `relink` already did.

## Decision: preserve the form
A rewrite keeps the author's brackets and replaces only the destination, which
is written with `encode_link_path` as before (`[x](<wall%20clock.md>)`). The
alternative, rewriting to `[x](wall%20clock.md)`, changes more of the line and
gains nothing, since both forms resolve. Brackets stay valid because
`encode_link_path` encodes `<` and `>`. The one behaviour change outside
angle links: a link whose text repeats its own destination, `[(a.md)](a.md)`
-style, no longer has the text edited by a move (`okf.py` quirk dropped).

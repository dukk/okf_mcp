# Issue #634: property test for the link-destination round trip

## Added
- `an_encoded_concept_name_round_trips_through_find_links_and_resolve_link`
  (`src/okf/markdown.rs`), a Hegel property (200 cases, derandomized). It
  builds a file name from random text and from reserved pieces (Unicode
  whitespace, controls, `%` and fake escapes, `:`, `<`, `>`, `(`, `)`, `[`,
  `]`, `\`, quotes, `#`, `?`, a scheme lookalike), keeps names that
  `rules::check_path` accepts as `d/<name>`, and asserts that both
  `[x](ENCODED)` and `[x](<ENCODED>)` parse as exactly one link whose target
  is `ENCODED`, and that `resolve_link` maps it back to the file.

## Notes
- The property found no round-trip bug.
- The optional `move_concept`/`relink` property was not added.
- Checked that it can fail: dropping `:`, narrowing whitespace to a space, or
  dropping `>` from `encode_link_path` each failed it (then reverted).

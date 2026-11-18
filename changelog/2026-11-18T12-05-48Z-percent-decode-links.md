# Percent-decode link destinations (issue #235)

## Added
- `markdown::encode_link_path`, the inverse of the decoding in `resolve_link`: it percent-encodes the characters a link destination cannot hold (controls, space, `%`, `#`, `?`, brackets, parentheses, `<`, `>`, `\`, quotes).
- Tests: `%20`, `%25`, non-ASCII and literal-`%` rows for `resolve_link`, an encode/decode round trip, a links-report test that a `%20` link to an existing file is not broken, and relink, move and repair tests for the written destinations.

## Changed
- `resolve_link` percent-decodes the path part of a destination (not the `#fragment`), once, so the link graph, backlinks, lint, the links report, the console renderer and the move, relink, delete and replay operations all read `a%20b.md` as the file `a b.md`.
- Operations that write a destination (move and relink rewrites, the moved concept's listing, split's stub and listing links, `repair_indexes` and `index` listings) percent-encode the path, so what they write resolves back to the same file.
- `repair_indexes` now lists names with spaces, `)`, `#` or tabs (written encoded) instead of noting that no link can be written; only an unwritable label is still skipped.
- The console renders `[x](a%20b.md)` as a link to `a b.md`.

## Notes
- A file literally named `a%20b.md` is now linked as `a%2520b.md`; an existing link written `a%20b.md` to such a file reads as the file `a b.md` and is reported broken if that does not exist. A `%` not followed by two hex digits (`100%.md`) stays literal.
- The URI-scheme test still reads the destination as written, so `mailto%3Aa` is a path.
- A destination with a space (`[x](a b.md)`) was never parsed as a link (a destination stops at whitespace), so such a file has to be linked as `a%20b.md`.

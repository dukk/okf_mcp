# Browse and search: titles, descriptions and plain snippets for the reader

## Added
- `GET /api/v1/catalogs/{c}/recent-changes`: each change has `title` and
  `description`, read from the published snapshot at request time; both are
  null for a deletion or a document that is gone since.
- `GET /api/v1/catalogs/{c}/tree`: each document has `description`.
- `GET /api/v1/search`: each hit has `snippet_text`: the text of the block
  holding the snippet's line, from that line on, read from the whole body
  with the console renderer's parser (`render::block_text`), then cut to
  160 characters with an ellipsis. It is built before any cut, so emphasis
  or a link crossing the cut, a reference link and a fenced code block all
  read as text. `snippet` and the MCP tool's output are unchanged
  (`SearchQuery::plain_snippets` is off for the tool, and the field is not
  serialized by `SearchHit`).

## Changed
- The shared Content-Security-Policy allows fonts from the server's own
  origin (`font-src 'self' https://fonts.gstatic.com`), for the reader's
  self-hosted fonts.

## Tests
- Tree descriptions; recent-changes titles and descriptions, null for a
  deletion; `block_text` and `excerpt`; plain snippets for emphasis and a
  link crossing the cut, a reference link, a fenced code block, a quote, a
  nested list item, a table row and a heading, and the fallback to the line
  itself; `snippet_text` on `/search`; the policy on `/ui/` and `/read/`.

## Docs
- `docs/operations.md`'s API table.

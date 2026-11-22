# get_concept: a missing section is a result, not a tool error (issue #907)

Time: see the file name (UTC).

## Why

- `get_concept` with a `section` that matched no heading (or several) returned
  `Refused`, shown by MCP clients as a failed call, though the read worked and the
  answer was "that section is not there, here are the ones that are".

## What changed

- `concept` (`src/mcp/documents.rs`) catches the `Refused` from `Outline::section` and
  returns the concept with `section_found: false`, the `outline` and a `note` (for no
  match, the requested heading; for ambiguous text, the matching slugs). `truncated` is
  true because no body is returned.
- New result field `section_found`: `true` when `section` named one heading, `false` on a
  miss, absent without `section`.
- `Outline::section` still returns `Refused`, so `split_concept` on a missing heading is
  still a refused action.
- Tool description and `docs/reference/tools.md` describe the new result.
- Tests: the repeated and unknown `section` cases now expect `Ok`; a new test covers a
  miss within the size limit.

## Not changed

- `read_document`, `get_concept` on a missing path and `history`: a per-tool judgement
  call, left for follow-up.

# Fix #426 review: repair skips listings no markdown link can reach

## Fixed

- `repair_indexes` and `okf-mcp catalog repair-indexes` listed every
  concept and folder by its raw path. A link's destination stops at
  whitespace or `)`, and `#` starts its fragment, with no escape
  (`okf::markdown`, pinned to `okf.py`), so a listing of `a note.md` or
  `my notes/` was never recognised again and each run appended another.
  Before writing a listing, the repair now parses the link it would write
  and checks that it resolves to the target (`links_back`). A path that
  fails is not listed, and a note names it: `did not list /<path> in
  /<index>: its path cannot be written as a markdown link`. Such a folder
  still gets its `index.md`, and linkable files in it are listed
  relatively.
- `insert_folder_listing`'s href choice moved into `folder_href`, shared
  with the check.

## Tests

- `repair_indexes_skips_listings_no_link_can_reach_and_settles`: names
  with a space, `)`, `#` and a tab, a folder with a space, an absolute-style
  root. The second run writes nothing. Confirmed to fail with the check
  disabled.

## Docs

- The repair's doc comment, the MCP tool description, the README and the
  operations guide say such paths are not listed.

## Not changed

- `index` and `create_concept`'s `index_summary` write the same raw link
  for such a path when asked to list it (#453).

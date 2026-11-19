## Added
- `ReplayOp::RewriteRawListing` (`"op": "rewrite_raw_listing"`, issue #717): fixes one raw (unencoded) listing bullet that versions before #453 wrote. It holds the concept, the bullet as written and its rewrite. Publish redoes it onto an index that changed meanwhile.
- `first_raw_listing` and `rewrite_raw_listing_text` in `src/okf/ops/replay.rs`. `lists_raw_target` is now `first_raw_listing(..).is_some()`, so detection and rewrite share one implementation of rules 0-3.
- Tests: an in-place rewrite of each raw form (space, `)`, `#`, `:`), relative and `/`-absolute, with a fragment, a summary and trailing text; a raw duplicate beside a working bullet, in either order and twice; two raw bullets; a name with a `%XX` escape and a link to another existing document left alone; idempotence; replay onto newer text (rewrite, removal, a working bullet gone meanwhile, an edited line, a parsed link); and a staged repair published after a concurrent edit of the index.

## Changed
- `repair_indexes`, and the operator's `okf-mcp catalog repair-indexes`, rewrite a raw concept listing in place. Only the destination changes: it becomes `encode_link_path` of the destination as written, relative or `/`-absolute. The label, any fragment and the text after the link are kept. When another bullet already links the concept, the raw bullet is removed instead. Each fix gets a note quoting the bullet, and the summary message counts the fixes (`fixed N raw listing(s)`).
- `index`'s note for a raw bullet now says that `repair_indexes` rewrites it.
- The `repair_indexes` tool description, `docs/reference/tools.md`, `README.md`, `docs/operations.md` and the CLI help describe the rewrite. They also drop the stale claim that a path with a space, `)` or `#` cannot be listed: since #453 such paths are percent-encoded.

## Notes
- The rewrite touches only a bullet `lists_raw_target` accepts. A bullet whose parsed link resolves to an existing document is never touched, nor is a name holding a valid `%XX` escape, nor a line that is not a legacy `* [label](destination` bullet.
- Publish replay cannot see which files exist, so it cannot re-check rule 3. It redoes a rewrite only while the raw line is still there byte for byte, and only when the link the parser reads in it resolves to no path at all (the space and `:` forms). A `)` or `#` form (`[A](a).md)` reads as a link to `a`) is left as written when the index changed meanwhile, for the next repair. A removal becomes a rewrite when the working bullet has gone meanwhile, so a listing is never lost.
- Only concept listings are rewritten. Folder listings (`# Sections`) are matched by `lists_target` alone, as before.

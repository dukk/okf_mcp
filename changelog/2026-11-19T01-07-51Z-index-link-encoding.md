## Added
- Tests (issue #453) for `index` and `repair_indexes` on names with a space, `)`, `#`, `%`, a no-break space and `:`: the bullet resolves, a second `index` writes nothing, and both tools write the same bullet.
- `lists_raw_target` and `already_lists` in `src/okf/ops/replay.rs`: recognise an older raw (unencoded) bullet for a concept.

## Changed
- `index`, `repair_indexes` and the publish replay treat an old raw bullet that no parser resolves as already listing the concept, so they no longer append a second bullet beside it. `index` adds a note that the old link does not resolve.
- `index` refuses, with a message, when the bullet it would write does not read back as a listing of the concept (the same round trip `repair_indexes` already checked).
- The `index` tool description and `docs/reference/tools.md` mention the encoding and the raw-bullet case.

## Notes
- A raw bullet is recognised only when its destination holds no valid `%XX` escape and does not resolve to another existing document, so `d/a%20b.md` is never taken as listed by the bullet for `d/a b.md`. A literal-percent name with a valid-looking escape gets its own bullet (`a%2520b.md`) rather than being matched to an old raw one.
- Since #632 `index` and `create_concept` with `index_summary` already wrote percent-encoded destinations that resolve; the new tests pin that.
- An old raw bullet is left as written: replay has no op that rewrites a destination in place. Its broken link stays visible to lint and the note names the fix.
- What import accepts is unchanged (the issue says that needs a decision).

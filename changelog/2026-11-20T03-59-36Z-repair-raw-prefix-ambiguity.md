## Added
- Regression tests for `repair_indexes` with `d/a b.md` and a longer `d/a b.md#extra.md` (and a `)` variant, and a folder index beside a concept named `index.md#x.md`): each target finds only its own bullet, whichever is looked at first.

## Changed
- `first_raw_listing` (shared by the concept pass and the `# Sections` folder pass of `repair_indexes`, and by the add-time duplicate check) no longer matches a raw bullet to a target when the bullet's destination also matches a strictly longer raw name of another existing document, ending at `)` or `#`. The longest name wins; the shorter target skips the bullet (never rewrites or counts it), and the longer target's own pass rewrites it, in any order. A catalog that cannot be listed counts as ambiguous, so the bullet is left alone.

## Notes
- Issue #734, a flaw in #730's code (issue #717): `* [Long](a b.md#extra.md)` was rewritten to `a%20b.md#extra.md`, pointing at the shorter concept. Publish replay is unchanged: it replays the decided `line` and `fixed`, and still leaves a line whose parsed link resolves to any path.

# M19 Task 07: the catalog picker and the folder tree

## Added
- `src/http/read/js/screens/catalogs.js`: the picker. It lists every
  reachable catalog (all pages) as buttons (`data-catalog`); with exactly one
  catalog, none chosen and no "gone" notice, it chooses that one and goes
  home. A catalog lost to a refusal (`#/c?gone=<catalog>`) shows a notice
  that its offline copies are deleted; an account with no catalog says so.
- `src/http/read/js/screens/tree.js`: one folder of the tree. The folder's
  `index.md` renders first; a deleted index (404 on the read, after the
  tree listed it) evicts its cached copy and the folders and concepts still
  render; offline without a copy is skipped the same way. Then its
  subfolders and its concepts by title. Logs are not listed.
- Journey `the_picker_and_the_tree_lead_from_a_catalog_to_its_concepts`
  (`tests/ui_e2e/read.rs`).

## Changed
- The plan's Task 7 carries a dated note: the journey is the fourth of
  `read.rs`, and `tree.js` re-checks `ctx.live()` after the index read's
  `catch` so a folder left during the eviction does not draw over the next
  screen.

## Review
- `tests/read_worker/tree.test.mjs` runs `screens/tree.js` in Node with
  controlled promises: a deleted index is evicted and the lists still
  render; an unavailable index is skipped with nothing evicted; any other
  refusal is thrown; and a folder left while the eviction is pending draws
  and reports nothing more (it fails without the `ctx.live()` re-check).
  No seam in `tree.js`: `esm-hooks.mjs` maps the imports of a module
  loaded with the `?doubles` query to `tests/read_worker/tree-doubles/`
  (a fake `h`, `read`, `evictDocument` and view parts).

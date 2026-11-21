# Reader: the folder screen

## Changed
- A folder shows breadcrumbs (the catalog's label, then folder titles), its
  title (its index's title, or its name) and its index's description as a
  lede, then an About row for the index's text, which is no longer drawn
  above the lists.
- Subfolders show an icon, their title, a one-line description, their page
  count (nested concepts included) and a chevron; concepts their title and
  a one-line description under "Pages · N"; the folder's log is a final
  "Log" row. The folder screen reads only the tree.
- A folder's index and log open as "About" and "Log".

## Tests
- `route.test.mjs`: the listing's titles, descriptions, counts and log.
  `tree.test.mjs`: what the folder draws from the tree, a refused tree, a
  late answer. Journeys: the picker and tree journey checks About and the
  Log row; a deleted folder index evicts its About copy (replacing the
  journey that deleted it under the folder screen).

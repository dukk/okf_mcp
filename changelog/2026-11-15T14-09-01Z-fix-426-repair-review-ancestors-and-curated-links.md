# Fix #426 review: repair creates missing ancestors and keeps curated folder links

## Fixed

- `repair_indexes` and `okf-mcp catalog repair-indexes` (both through
  `okf::ops::log_index::repair_counted`) now collect every ancestor of a
  folder holding a concept or an index, not only each file's own folder.
  A catalog with `infra/hosts/index.md` but no `infra/index.md` failed with
  `NotFound` reading the missing parent; now `infra/index.md` is created
  and listed in the root, and `hosts/` is listed in it.
- A folder index the repair creates is no longer listed through
  `ensure_index`, which inserts the parent's listing unconditionally. The
  repair creates the index itself, then lists the folder only when the
  parent links it in no wording, so a curated `* [Our systems](infra/)`
  is not joined by a second `* [Infra](infra/) - TODO` bullet.

## Changed

- The note for a folder index the repair creates reads
  `created /<folder>/index.md; replace its TODO line`; its listing, when
  one is added, gets its own `listed /<folder>/ in ...` note. The TODO
  placeholder count is unchanged.

## Tests

- `repair_indexes_creates_the_missing_ancestors_of_a_nested_index`
- `repair_indexes_keeps_a_curated_link_to_a_folder_whose_index_is_missing`

Both assert the curated text is kept, one listing per target, and a no-op
second run.

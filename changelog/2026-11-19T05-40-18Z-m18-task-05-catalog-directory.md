# M18 task 5: the catalog directory

## Added

- `src/cache/directory.rs`: `CatalogDirectory`, catalog rows by name with their access facts (owner, suspension, maintenance opt-out, audit team id).
  - `resolve(name)`: one read however many callers ask together, then served for `catalog_ttl`.
  - Absent names are remembered for 5 s in a second `Bounded<String, ()>` (the row cache's `check` cannot expire a fresh entry). A `CatalogMeta` message drops both entries of its name; a lookup that read "no such catalog" before a create is not remembered (a counter read before the shared read starts and carried with its result, so followers that join an overtaken read judge it by the same counter).
  - `resolve_many(names)`: cached rows plus one query for the rest; the rows read are not cached, so a listing cannot push request entries out.
  - `apply` and `mark_all_stale` for the change feed.
- `store::catalogs::{CatalogRow, SuspendedRow, row_by_name, rows_by_names, page}`.
- `Bounded::peek_fresh`: a current entry without loading or reordering.
- Config `catalog_directory_entries` (100000) and `catalog_ttl` ("10s"), documented in `docs/operations.md`.
- `App::directory()`.

## Changed

- `App::apply` and `App::apply_reconnect` feed the directory first.
- The M18 plan's Task 5 is corrected (see its note).

## Notes

- Nothing but tests reads the directory yet.
- A `Catalog` message drops a cached row behind the new generation by scanning the resident rows (bounded by `catalog_directory_entries`) and, through the cache's fence, keeps every read running at that moment out of the cache. No `id -> name` or minimum-generation map is kept: that state would need its own bound.
- `invalidate.rs` is unchanged: `apply` lives on `App`.

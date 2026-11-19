# M18 Task 10: listings are queries; the search fan-out cap

Issue #688, plan `docs/superpowers/plans/2026-11-18-okf-mcp-m18-bounded-memory.md` Task 10,
spec Decisions 7 and 8.

## Added

- `CatalogDirectory::reachable_page` and `offered_page` (`src/cache/directory.rs`): one page
  of the catalogs a principal reaches, by name, narrowed to an MCP connection's list when it
  has one. A granted key (or a connection list) reads its bounded set with `resolve_many`; a
  key that reaches every catalog reads `limit + 1` rows after the cursor. The rows read are
  not cached. `Page<T> { items, next }`, `page_size` (default 100, at most 1000) and
  `parse_cursor` (an opaque, unpadded base64url cursor; anything else is a 400 naming
  `after`, before any read).
- `?after=&limit=` and a `next` field (absent on the last page) on `GET /api/v1/catalogs`,
  `GET /api/v1/whoami` and `GET /api/v1/labels`; an `after` argument and a `next` field on
  MCP `list_catalogs`.
- `search_fanout_max` (default 25, from 1 to 999): `GET /api/v1/search` without `catalogs`
  refuses, with a 400, a credential that reaches more catalogs than that.
- `changesets::open_for_catalogs`: `list_changesets` without `catalog` reads the open
  changesets of the catalogs it names (all of them for a key that reaches every catalog) in
  one query, then keeps each row only if the connection offers its catalog, it is not
  suspended for the caller, and the gates bound to that row's `catalogs.id` admit it
  (`ToolContext::check_contents_by_id`).
- `App::loaded_catalog(&CatalogRow)`: the loaded state of the incarnation a row describes,
  for a listing's snapshot fields. Task 8 replaces it.
- Console: `api.pages(path, field)` follows `next` for the views that need a whole list (the
  catalog switcher and labels, the unlock and entitlement pickers, import and export). The
  Catalogs page draws one page with Load more, keeping its rows and cursor across a failed or
  overtaken reload (lessons.md), and its maintenance drawer reads pages until one holds the
  catalog.
- Tests: `cache::directory::listing_tests` (paging both reaches, a connection list, cursors
  that name hidden or missing catalogs, malformed cursors), `http::api::listing_tests` (the
  three HTTP listings paged 3, 3, 1; one page for a granted key; 400 for malformed cursors;
  the fan-out cap at 25 and at a configured value), `mcp::catalogs` (`after`),
  `store::changesets` (`open_for_catalogs`), and the browser journey
  `the_catalogs_page_loads_more_and_keeps_its_cursor_when_reloads_fail`.

## Changed

- No listing enumerates the catalog map any more: `GET /catalogs`, whoami, labels, search,
  `list_catalogs` and `list_changesets` read the directory or the database. Description,
  display name, label and owner come from the catalog's row; files, concepts, generation
  and last change from the loaded snapshot of the same incarnation; suspension and
  maintenance from the principal, as before.
- `GET /api/v1/labels` pages its `catalogs` for every caller, the every-catalog admin
  included. `Labels::catalog_slugs` is gone.
- `ToolContext::admission_to` is now `admission_by_id(name, catalogs.id, access)`.
- Response shapes (release notes): the four listings answer one page with `next`; a client
  that read everything from one answer must follow `next`. `GET /api/v1/search` without
  `catalogs` can now answer 400.

## Notes

- `list_changesets` without `catalog` now also lists the changesets of a catalog this replica
  has not loaded yet; each still passes the same reach, suspension and incarnation-bound
  admission checks.
- `limit` and `after` apply to granted keys too, not only to keys that reach every catalog,
  so `?limit=` means the same for everyone (plan corrected).
- The fan-out cap counts the catalogs a credential reaches, not those open to it now, so
  suspending a team or ending an unlock does not move a key across it.
- Ordering is by name in each source's own collation (Postgres for every-catalog pages,
  bytewise for a granted set), and the cursor is compared in the same place, so pages
  neither skip nor repeat.
- `docs/operations.md`: the new setting, "Catalog listings are paginated", and the changed
  API rows. Task 17 writes the rest of the prose.

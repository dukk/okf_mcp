# M18 Task 8: catalogs load on demand into a bounded cache

Issue #686, part of epic #678 (milestone 18, bounded memory). Spec
`docs/superpowers/specs/2026-11-18-okf-bounded-memory-design.md`, Decisions
2, 3 and 11.

## Added

- `cache::catalogs::CatalogCache` (`src/cache/catalogs.rs`): catalog
  snapshots by `catalogs.id` in a `Bounded` cache limited by
  `catalog_cache_bytes` (default 1 GiB) of estimated bytes. `get(row, ..)`
  loads once per miss (single flight), and refreshes a cached state in
  place when the directory row's generation is ahead of its snapshot.
  `catch_up(id, generation, ..)` serves the change feed, M16 announcements
  and the HTTP import: it refreshes a resident entry and, for one that is
  not resident, keeps a load in flight out of the cache (Decision 11's
  minimum generation, through the cache's epoch). Every in-place refresh
  re-weighs the entry, so a catalog that grows past the budget is dropped.
- A negative cache of `(id, generation)` pairs whose stored data does not
  build: that generation is built once, not on every request.
- The weight estimate: 1 KiB per catalog, plus 1 KiB per file, plus 8
  bytes per stored byte of content, measured with a counting allocator in
  `tests/snapshot_weight.rs` (ignored by default) on synthetic catalogs of
  500 and 5,000 documents and of 1 and 5,000 empty documents. Method and
  numbers are in spec Decision 2. An empty catalog, or one of many empty
  files, is charged for its files (review of PR #714).
- `catalogs::row_by_id`, the directory row of one incarnation by id
  (`resolve_changeset` uses it instead of scanning the map).
- `App::catalog_row`, `App::catalog_state`, `App::listed_state`,
  `App::catalog_cache`, and an async `App::catalog(name)`.
- `ToolContext::snapshot(&state)`: one request reads one snapshot per
  catalog (Decision 3), even when the catalog is refreshed in place or
  evicted meanwhile.
- `Bounded::entry_count`.
- Tests: `start_up_loads_no_catalog`, `the_first_read_loads_and_the_second_hits`,
  `a_catalog_invalidation_refreshes_in_place`,
  `a_broken_generation_is_not_rebuilt_on_every_request`,
  `a_request_keeps_its_snapshot_while_the_entry_is_evicted`,
  `a_request_sees_one_snapshot_when_the_catalog_refreshes_between_reads`,
  `a_resident_catalog_that_grows_past_the_budget_is_not_kept`,
  `a_load_overtaken_by_a_catalog_message_is_served_but_not_kept`, and
  others in `src/cache/catalogs_tests.rs`; in `tests/multi_replica.rs`,
  `a_catalog_created_on_one_replica_is_readable_on_the_other`.

## Changed

- Start-up loads no catalog: `App::load` reads the keys and settings and
  checks `default_catalog` with one directory read.
- The `Catalogs` map, `App::catalogs()`, `ToolContext::catalogs()`,
  `App::loaded_catalog` and the poll's broken-catalog map are gone. The
  poll loop only reloads the key cache; catalogs are refreshed by the
  change feed and checked against their directory row on every use.
- `CatalogState` no longer holds the description or owner (read them from
  the `CatalogRow`), and `CatalogState::load` takes the row.
- `GET /catalogs` and MCP `list_catalogs` load the listed page's catalogs
  through the cache and leave out one that does not build; `/api/search`
  loads the catalogs it searches; `GET /whoami` now lists every reachable
  catalog name without loading any.
- `/healthz` reports `cached_catalogs` (how many catalogs this replica holds
  now) instead of `catalogs`.
- The per-catalog gauges `okf_catalog_concepts` and `okf_catalog_generation`
  are removed, with their Grafana panels; nothing records
  `okf_catalog_refreshes_total` any more (Task 16 removes the family).
- After a lost change feed (or an M16 resubscribe), the directory is marked
  stale instead of every loaded catalog being refreshed at once.
- Multi-replica test servers also set `catalog_ttl` to one hour.
- A publish has the cache re-weigh its catalog on every outcome, including
  a refusal after its preliminary refresh (review of PR #714).
- HTTP writes are read-your-writes on the replica that made them (every
  route, through a layer on the top-level router; the archive import also
  applies its own `catalog` invalidation itself): the routes collect the invalidations their writes queued
  (`invalidate::capturing`) and drop the affected directory rows before
  answering (`App::apply_own_writes`), instead of waiting for the change
  feed. The operations guide's recovery text now describes the change feed
  and `catalog_ttl` as the bounds, not `poll_interval`.

## Notes

- The plan's Task 8 has a "Corrected (issue #686)" note listing where the
  code differs (no `CatalogCache::mark_all_stale`; `apply` is `catch_up`;
  `snapshot(&state)`; the whoami behaviour); Tasks 9 and 16 and the
  self-review's name list were updated to match.
- No real catalog checkout was available to the weight measurement; the
  synthetic documents are prose-heavy, so a catalog of shorter, more linked
  documents may weigh somewhat more per byte.
- Listings load their page's catalogs one after another; at the default
  page of 100 cold catalogs this is the plan's accepted cost.

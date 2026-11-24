# M21 Task 18: reads across stores, and `unavailable` (issue #977)

## Added

- `unavailable` (catalog ids, sorted, each once, omitted when empty) on `search`, `list_catalogs`, `GET /catalogs`, `list_changesets` without `catalog` and the console search (`GET /search`): the catalogs left out because their store could not be reached. Only catalogs the caller reaches (and the connection offers) are ever named.
- `ReadSet` and `ToolContext::{read_set, states_or_unavailable, stores_main_first}`; `CatalogDirectory::resolve_or_unavailable`, `Lookup`, `Page::unavailable`; `Stores::connect_timeout`; `stores::READ_CONCURRENCY` (8); `catalogs::rows_on_store`.
- Tests in `src/mcp/unavailable_tests.rs` that make a store really unreachable (its row points at a closed port, its pool dropped), through `TestApp::catalog_on_new_store` and `TestApp::make_unreachable`.

## Changed

- First-match reads (`get_concept`, `read_document`, `history` without `catalog`) return `StoreUnavailable` when a catalog earlier in the selection is on a lost store and no earlier catalog answers (plan P3). Later catalogs are never consulted.
- A changeset handle is looked up in every store, `main` first; a lost store ends the lookup with `StoreUnavailable`. `list_changesets` without `catalog` reads every store, at most 8 at a time, each within `store_connect_timeout`.
- The directory no longer fails a listing or a credential's ceiling because one store is down; the generation probe asks the stores together and is bounded the same way. A call probes a lost store once, not once per catalog.
- `futures-util` gains the `alloc` feature (already in the lockfile).

## Notes

- A store that is down costs a call at most one `store_connect_timeout` (5 s by default) probe: the page reads of the console search and `list_catalogs` tell the call's `ToolContext` which stores are down, so the selection and the state loads do not probe them again. A failed open is still not remembered by the registry, so every call retries.
- Review round 1: a store that goes away after its pool opened (a transport failure from an established or re-acquired connection) is classified as `StoreUnavailable` on the catalog state load and refresh path (`CatalogCache::get`), and that load is bounded by 6 times `store_connect_timeout` (`Stores::state_load_timeout`), so a stalled query is unavailable too. `StoreUnavailable` is no longer remembered as a catalog whose data does not build. Other store reads that report `Database` for a transport failure stay in #995.
- Review round 2: a refresh that ends as `StoreUnavailable` propagates (search names the catalog, a first-match read fails) instead of serving the stale snapshot; callers that shared a failed `Bounded::get_or_load` receive `StoreUnavailable` when the leader's failure was one (still `Busy` otherwise), so concurrent searches on a lost store all return partial results; `GET /api/v1/catalogs` uses a per-request down set, so a failing store costs one load bound. Four regression tests in `src/mcp/unavailable_tests.rs`.
- `cited_by` still reads strictly and fails with `StoreUnavailable`; a follow-up can make it merge.
- `docs/reference/tools.md` gets the field in three tool descriptions; Task 19 writes the prose.
- Plan Task 18 carries a dated correction note.

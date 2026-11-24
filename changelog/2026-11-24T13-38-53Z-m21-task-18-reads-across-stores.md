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

- A store that is down costs a call at most one `store_connect_timeout` (5 s by default) per call; a failed open is still not remembered by the registry, so every call retries.
- `cited_by` still reads strictly and fails with `StoreUnavailable`; a follow-up can make it merge.
- `docs/reference/tools.md` gets the field in three tool descriptions; Task 19 writes the prose.
- Plan Task 18 carries a dated correction note.

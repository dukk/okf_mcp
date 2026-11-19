# M18 task 7: `ToolContext` resolves catalogs asynchronously

Issue #685, part of epic #678 (bounded memory).

## Added
- `ToolContext::state` returns `CatalogNotSelected` for a name missing from the call's map, instead of panicking.

## Changed
- `ToolContext::select`, `check_connection`, `states`, `read_set`, `one` and `report_catalog` are `async`; every caller `.await`s (MCP tools, maintenance tools, browse routes, `list_catalogs`' `list`).
- Behaviour is unchanged: resolution still reads the map copied when the context was built.

## Notes
- This moves the `.await`s now so the next task (`CatalogCache`) changes only where resolution reads from.
- `selection::resolve` keeps its sync `exists` closure over the map; see the plan's correction note under Task 7.

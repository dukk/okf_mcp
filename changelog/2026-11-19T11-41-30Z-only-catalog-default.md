## Added
- With no argument, connection list, key default or server default, a key granted exactly one catalog that exists now selects it (issue #663): bare `search`, `read_document`, `get_concept` and `open_changeset` use it, and `list_catalogs` shows it as the selection and write target.
- Unit tests in `src/selection.rs` for one reachable catalog, one reachable but closed, several reachable, and `Reach::All`.

## Changed
- The "no catalog selected" refusal leads with the fix a client can make, "pass `catalog` (see `list_catalogs`)", before the operator's options.
- The server instructions, the `list_catalogs` description, the `catalog`/`catalogs` parameter docs, `docs/reference/tools.md`, the README and `docs/operations.md` describe the fallback and say to pass `catalog` when `list_catalogs` shows an empty selection.

## Notes
- A key granted every catalog (`Reach::All`) never falls back, even when only one catalog exists: an admin-reach key silently selecting a catalog would be surprising, and a second catalog created later would change what its bare calls read.
- The fallback counts reachable catalogs that exist, open or closed. A sole closed catalog (suspended, unlock required, maintenance off) is refused exactly as a key default is; with several reachable catalogs, closing all but one does not make that one the selection.
- The fallback never widens access: it picks only a name in the key's own grants, and runs only after every other source is empty.

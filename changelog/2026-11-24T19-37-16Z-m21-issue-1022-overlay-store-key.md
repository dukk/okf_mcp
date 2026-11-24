# M21 issue 1022: overlay cache keyed by store and changeset

Brooke's review of the milestone PR found that the overlay cache, its build
lock and the per-changeset write queue were keyed by the bare
`changesets.id`. Since M21 each store numbers changesets from its own
sequence, so two stores hand out the same id. A read through one catalog's
handle could be served another catalog's cached overlay when the generation
and version matched, and `get_in` could apply an edit to the wrong workspace.

## What changed

- `store::changesets::ChangesetKey { catalog_id, id }` names a changeset
  across stores (`ChangesetRow::key()`). Catalog ids are unique server wide
  (the registry assigns them), so the pair is unique.
- `Overlays` caches, locks builds (`builds`), replaces and invalidates
  (`forget`) by that key. `Overlay` carries its key; `forget` takes a key.
- `KeyedLocks` is generic over its key (default `i64`, for catalog ids); the
  write queue and the overlay build lock use `ChangesetKey`, so two stores'
  changesets no longer queue behind each other.
- No cross-replica invalidation message names a changeset (messages name a
  store, a catalog, a team or a user), so none needed changing.

## Other per-store identities checked

- Catalog cache, `broken`, rebuild, publish and `catching_up` locks: keyed by
  catalog id, which is global. Unchanged.
- Credential, directory and label caches: keyed by hashes, names or labels in
  the accounts database, not per store. Unchanged.
- Documents, revisions and entries are only read by `(catalog_id, ...)` or
  through a changeset row, never held in an in-memory map by bare id.

## Tests

- `mcp::overlay_stores_tests`: two stores, colliding changeset ids, equal
  generation and version, two principals each reaching one catalog,
  alternating cached reads and an edit through `get_in`. It failed before
  the change (the remote read showed the home draft).
- `overlay::tests`: one changeset id in two catalogs is cached and forgotten
  apart.

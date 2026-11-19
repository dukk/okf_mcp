# Per-catalog locks outside the snapshot entry (M18 task 6)

## Added
- `App::publish_lock(catalog_id)`: the in-process half of a catalog's publish lock, a `KeyedGuard` from `App`'s `publish_locks` (`KeyedLocks`, keyed by catalog id).
- `App::rebuild_locks()`: the catalog rebuild locks (`KeyedLocks`, keyed by catalog id) that every snapshot refresh takes.
- Tests: `store::publish::tests::publishes_of_one_catalog_serialize_across_two_states` (a publish through a second `CatalogState` of a catalog waits for the publish lock held for the first) and `catalog::tests::refreshes_of_two_states_share_the_rebuild_lock`.

## Changed
- `CatalogState` holds no mutex: its `rebuilding` and `publishing` fields and `CatalogState::publish_lock()` are gone (spec M18 Decision 4). Two states of one catalog, as an evicted cache entry still in use and its reload will be from Task 8 on, share both locks.
- `CatalogState::refresh`, `refresh_locked` and `current_covering`, and `Overlays::get`, take the rebuild locks (`&KeyedLocks`) as an argument; callers pass `app.rebuild_locks()`.
- `store::publish` and `discard_changeset` take `app.publish_lock(state.id())` where they locked the state's mutex, at the same point: before any pooled connection or Postgres lock, so the lock order against the advisory lock and the user, catalog and changeset rows is unchanged.
- The M18 plan's Task 6 carries a correction note, and Task 1's `apply_reconnect` sketch passes the rebuild locks.

## Notes
- Issue #684. Behaviour is unchanged while each catalog has one `CatalogState`, as today; the change matters once Task 8's bounded cache can evict and reload one.
- The publish and rebuild locks stay separate maps: a publish holds its publish lock through the refresh after commit, which takes the rebuild lock.
- `KeyedLocks` drops a key's entry when nobody holds or waits for it, so the two maps hold only catalogs being published or rebuilt.

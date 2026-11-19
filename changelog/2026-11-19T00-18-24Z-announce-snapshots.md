# Catalog snapshots are announced across replicas (best effort)

## Added
- Catalog generation announcements over Redis (`{p}catalog-generation`, payload `<catalog id> <generation>`). A publish and an HTTP import announce after they commit, on a detached task; the command line announces each `import` and `catalog repair-indexes` it committed (even when a later catalog in a repair batch fails).
- A catalog subscriber on every serving replica (`shared::announce::listen_catalogs`). It refreshes the named catalog through `CatalogState::refresh`, so a generation already reached is skipped and concurrent refreshes (announcement, poll, local publish) build once under the catalog's rebuild lock. It catches up on every loaded catalog when it (re)subscribes, and leaves a catalog it has not loaded, or has deleted, to the poll loop. It shares the `admin_generation` subscriber's loop (reconnect, backoff, grace) through a `Handler` trait.
- `okf_catalog_announcements_total{result}` (`applied`, `current`, `unknown`, `failed`) and an `okf.catalog.announcement` root span per announcement received. The example Grafana dashboard's catalog refresh panel shows it too.
- `CatalogState::current_covering`: the snapshot, refreshed first when a changeset entry's base revision is newer than it.

## Changed
- `pending_changes` and reads through a changeset (`Overlays::get`) refresh a snapshot that is behind an entry's base before diffing or building the overlay. `pending_changes` never reports `current_revision < base_revision`: if the snapshot is still behind after the refresh it answers `Busy`.
- `docs/operations.md` "Several replicas": catalog reads are eventually consistent across replicas (normally sub-second through the announcement, `poll_interval` after a lost one; no read-your-writes guarantee); the Redis section lists the new channel, what it does and how it fails; the metric and span references list the new series and span. The multi-replica design spec's non-goal carries a correction.

## Notes
- Issue #659. Propagation is best effort: a write's answer does not wait for the announcement or the other replicas' refresh, so a client's next read on another replica may still be old. Strict read-your-writes would need a read-side generation barrier, left as a separate decision. The changeset tools do get a guarantee for their own entries (the base refresh above).
- A cached overlay built on a snapshot behind its entries (a write's `get_in` on a lagging replica caches one, even when the write is refused) is never reused by a read, which rebuilds on a refreshed snapshot.
- Without `[redis]` nothing changes: no subscriber starts and nothing is published.
- A catalog created or deleted on one replica still reaches the others at their next poll (the poll loop owns the catalog map); deletion and suspension already reach the key cache through `admin_generation`.

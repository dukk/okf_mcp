# M18 task 9: overlays weighed in bytes (issue #687)

## Added
- `overlay_cache_bytes` (default 256 MiB): the most memory the changeset overlays may take, estimated with the catalog snapshot weigher (`cache::catalogs::weight`) over the overlay's whole workspace (the base's files plus the pending entries), so an overlay of many empty files is not free.
- `Bounded::with_idle` (time to idle) and `Bounded::insert`; `MemWorkspace::totals`.
- Overlays report `okf.cache.*` with `cache="overlay"`.

## Changed
- `Overlays` is a `Bounded<i64, Arc<Overlay>>` instead of a hand-rolled count-limited LRU; idle entries go after `session_idle_timeout` (moka's time to idle). `forget` is async.
- `overlay_cache_size` is deprecated: it still parses, is ignored, and start-up warns.

## Notes
- An `Overlay` is immutable, so its weight is computed once at build; every path that grows an overlay builds a new one (a changeset write bumps the version, a publish moves the generation) and goes through `store`.
- The `Arc<Loaded>` base an overlay pins is not charged again (its snapshot): the catalog cache weighs it while it is cached. A superseded base held only by an overlay is not counted (bounded by the number of cached overlays).
- Which overlay moka drops over budget follows its admission policy, not strict LRU.

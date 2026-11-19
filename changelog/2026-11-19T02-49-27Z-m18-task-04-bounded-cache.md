# M18 task 4: `cache::Bounded<K, V>`

## Added

- `moka` 0.12 (`future` feature), resolved to 0.12.16 in `Cargo.lock`.
- `src/cache/bounded.rs`: `Bounded<K, V>`, a bounded async cache over moka with
  - single-flight loads (`get_or_load`; a caller that shared a failed load gets `Busy`);
  - per-key invalidation epochs kept only while a load is in flight, so invalidating keys nobody is loading keeps no state;
  - a cache-wide fence moved by `invalidate_where`, so loads in flight are not kept either;
  - stale marking (`mark_all_stale`, `get_or_revalidate`);
  - `reweigh`, for values whose weight changes behind an `Arc`;
  - over-budget values served and not kept, warned about at most once an hour per cache.
- Metrics `okf.cache.requests`, `okf.cache.evictions`, `okf.cache.load.duration`, and the gauges `okf.cache.entries` and `okf.cache.bytes` (`Metrics::cache_*`, `Metrics::observe_cache`). Labels are `cache`, `result`, `cause` and `outcome` only.

## Changed

- `src/lib.rs` declares `pub mod cache`.
- The M18 plan's Task 4 and the moka facts are corrected (async `invalidate_where`, `check` runs only for stale entries, no minimum generations yet); Tasks 5 and 8 were told.

## Notes

- Nothing uses the cache yet; Tasks 5 and 8 build the directory and catalog caches on it.
- An invalidation that lands in the microseconds between moka inserting a load's value and the leader's check is seen by that check, which removes the entry again.
- Minimum generations (spec Decision 11) are left to Task 8, since `Bounded` cannot read a generation out of its value type.

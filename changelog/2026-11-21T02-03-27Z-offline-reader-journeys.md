# Offline reader journeys (issue #817, M19.10)

Browser journeys in `tests/ui_e2e/read.rs` for the reader's offline behaviour:
viewed concepts readable offline and unviewed ones saying so, the first concept of a
first visit, the 200-concept content cap, shell caches of other builds dropped by a new worker,
deleting offline copies without a connection, a full storage quota, denied Cache Storage, the
short/full copy fallback, and coming back online. No `sw.js` or page code needed a fix.

Harness (`tests/ui_e2e/harness.rs`):

- `limit_storage` (`Storage.overrideQuotaForOrigin`) and `deny_cache_storage` (a script on every
  new document plus `Runtime.evaluate` in the page and service-worker targets). Both worked as
  the plan assumed; each journey asserts that the override refuses writes.
- `go_offline`'s guard now restores `offline: false` on every target it changed when dropped.
  `coming_back_online_reads_fresh_content_again` covers it.
- `open_reader` waits for the hash to settle; a reload straight after a hash-only navigation
  could reload the previous route and made journeys flaky under load.

The plan's Task 10 carries a dated correction note.

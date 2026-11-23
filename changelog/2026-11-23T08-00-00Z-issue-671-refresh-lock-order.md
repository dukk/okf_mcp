# Issue #671: the catalog refresh takes its connection before the rebuild lock

## Problem

`CatalogState::refresh_to` took the catalog's rebuild lock and then waited
for a pooled connection to build the new generation. A publish's
`refresh_locked` already holds a connection (its transaction's) and then
waits for the same lock. On an exhausted pool each held what the other
needed, until the pool's acquire timeout fired. The change-feed poll, and
since #669 the Redis announcement listener, refresh through the same path.

## Changed

- One order everywhere: a pooled connection first, then the rebuild lock.
  `CatalogState::refresh` now goes through a new `refresh_at`, which starts
  the build's read-only transaction (taking the connection) before
  `refresh_to` takes the lock. If the wait shows another refresh already
  reached the generation, the unused transaction is rolled back. The
  holder of the rebuild lock never waits for the pool.
- `build` is split into `begin_read` and `build_in`; `build` (used by
  `CatalogState::load`) is unchanged in behaviour.
- `refresh_locked` is unchanged: it already holds a connection.

## Why not `try_lock` in `refresh_locked`

A publish must see its own write before it returns: `current_base` checks
that the snapshot's generation matches the database's, under the catalog's
advisory lock, and answers `Busy` otherwise. Skipping the rebuild when the
lock is busy would hand that to the holder, whose generation read may
predate the publish. Taking the connection first keeps the publish's own
rebuild, on its own connection.

## Tests

- `catalog::tests::a_refresh_waiting_for_a_connection_does_not_hold_up_a_publish_refresh`:
  pool of 2 with an 8 s acquire timeout; the "publish" holds one connection,
  the test the other, and a refresh is queued for a connection. The publish's
  `refresh_locked` must finish within 4 s and the queued refresh once a
  connection frees. With the old order the publish waits for the lock until
  the acquire timeout and the test fails.

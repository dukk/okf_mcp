# Issue #671: a publish's refresh no longer waits for the rebuild lock

## Problem

`CatalogState::refresh_to` took the catalog's rebuild lock and then waited
for a pooled connection to build the new generation. A publish's
`refresh_locked` already holds a connection (its transaction's) and then
waited for the same lock. On an exhausted pool each held what the other
needed, until the pool's acquire timeout fired. The change-feed poll, and
since #669 the Redis announcement listener, refresh through the same path.

## Changed

- One rule everywhere: nothing that holds a pooled connection waits for the
  rebuild lock. Refreshers (`refresh`, from the poll, an announcement, a
  request) still take the lock and then a connection, so a holder of the
  lock may wait for the pool. `refresh_locked`, the publish's refresh on its
  own connection, no longer takes the lock (and no longer takes `rebuilds`).
- `refresh_locked` builds the generation it read, under the catalog's
  advisory lock, on its connection and swaps it in through the new
  `swap_if_newer`. Refreshers swap through it too, so a snapshot never
  moves back when a refresher's older build finishes after a publish's.
  Both can build the same generation once each; that is cheap next to a
  stall as long as the acquire timeout.

## Read-your-writes

A publish sees its own write before it returns. `refresh_locked` always
builds its own generation on its own connection and swaps it in unless a
newer one is there (never an older one), and `current_base` still checks the
snapshot's generation against the database's and answers `Busy` otherwise.
A `try_lock` that skipped the publish's rebuild would have left that to a
lock holder whose generation read may predate the publish.

## Tests

- `catalog::tests::a_refresh_waiting_for_a_connection_does_not_hold_up_a_publish_refresh`:
  pool of 2 with an 8 s acquire timeout; the test holds one connection as the
  publisher and one more, and a refresh holds the rebuild lock and is queued
  for a connection. The publish's `refresh_locked` must finish within 4 s
  and the refresh once a connection frees. With `refresh_locked` waiting for
  the lock again, the publish waits until the acquire timeout and the test
  fails.
- `catalog::tests::a_swap_never_moves_the_snapshot_back`.

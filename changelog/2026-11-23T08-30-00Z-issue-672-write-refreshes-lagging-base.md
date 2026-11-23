# Issue #672: a write on a lagging replica records the current base

## Problem

`write::apply` took a path's base revision from the snapshot its overlay was
built on. On a replica that was not told of a publish elsewhere, that
snapshot was older than the database, so the entry stored an older
`base_revision` (and the edit was made on older text), and publish then
replayed or reported a conflict a fresh base would have avoided.

## Changed

- `write::apply_audited` refreshes the catalog's snapshot (one generation
  read, a rebuild only if it moved) after the changeset's write queue and
  before its transaction begins, then re-weighs the cache entry if it
  moved. Nothing holds a pooled connection at that point, so the rebuild
  lock order of issue #671 holds.
- Test `an_edit_on_a_lagging_replica_records_the_fresh_base_and_publishes_cleanly`.

## Gap 1 of the issue (new catalogs not announced): already handled

Re-checked against the current code: there is no poll loop. Creating a
catalog sends `Invalidation::CatalogMeta`, which drops the directory's row
and absent-name entries on every replica (`CatalogDirectory::apply`), and a
catalog is loaded on its first use (`CatalogCache::get`). A replica misses a
new catalog only if it lost the message (then until the absent entry's 5 s
TTL, or a resubscribe's catch-up, `App::catch_up_catalogs`). No new message
type is needed. The multi-replica spec carries a dated correction.

## Existing test changed

`a_revert_on_a_stale_snapshot_keeps_the_older_base_and_conflicts` asserted
the behaviour this issue calls a bug (a revert after an unseen import based
on the stale revision and conflicted). It is now
`a_revert_on_a_stale_snapshot_refreshes_and_takes_the_current_base`: the
base is the current revision and the publish succeeds.

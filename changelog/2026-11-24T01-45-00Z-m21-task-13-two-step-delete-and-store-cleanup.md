# M21 Task 13: deletion in two steps, and cleanup on every store

Issue #972. A catalog or key deletion commits on the account database and
owes the rest to the store its data lives on. No migration: the tables
(`catalogs.deleted_at`, `catalog.catalog_state.deleted_at`, `store_cleanups`)
came with 0041.

## Added

- `src/store/cleanups.rs` and the housekeeping task `CleanupStores`
  (`cleanup_stores`, every minute, once per cluster). It drains
  `store_cleanups` one store at a time, up to 100 rows per store per run. A
  store that cannot be opened, or a row that fails, counts an attempt and
  waits for the next run without holding up other stores; past 20 attempts a
  row is logged at error level on every run and kept. A catalog's cleanup
  deletes its `catalog_state` row (the cascade takes its data; its audit
  entries stay, P5) and then its registry row (the name is free again, P6).
  A key's cleanup discards its open changesets on that store (audited as
  `catalog.changeset_discarded`, reason `key_deleted`, actor
  `system:store-cleanup`) and clears the key id from the store's changesets
  and maintenance runs. A backstop owes the cleanup of any deleted catalog
  that has none pending.
- `catalogs::delete_pause` (test only): a pair of barriers inside
  `delete_on` for the race tests.

## Changed

- `catalogs::delete_on` follows the global lock order of `CatalogDb::begin`
  (account connection, account row locks, store connection). The account
  transaction takes the people, default-catalog keys, registry row and
  store row first; the store transaction takes the advisory lock, the
  `catalog_state` row and the open changesets, refuses unpublished work
  without `force`, discards with it, and marks `deleted_at`; the account
  writes (`catalog.deleted`, `catalogs.deleted_at`, the `store_cleanups` row)
  go on the transaction that already holds its rows, and the account commits
  first. This differs from P12's text, which had the store transaction wrap
  the account one: that order waits on an account row while holding the store
  row, which a writer may hold the other way round. P12's guarantee holds the
  same way: a writer queued on the lock finds `deleted_at` and is refused
  with `CATALOG_GONE`; one that landed first holds the delete off and is seen
  by it.
- `users::delete`, `delete_own` and `delete_in` no longer open a store
  connection: a deletion writes one cleanup row per key per store in its own
  transaction. `user.deleted` no longer records `changesets_discarded`.
- `open_changeset` takes `FOR KEY SHARE` on its key's row on the account
  transaction before the store connection, so a key's deletion and an open
  serialise as the dropped foreign key did; a deleted key is refused.
- Every registry read that filtered on `provisioned_at IS NOT NULL` also
  filters `deleted_at IS NULL`; a test fails when one does not.

## Notes

- A user or team that owns a deleted catalog cannot be deleted until its
  cleanup has removed the registry row (`catalogs.owner_team_id` is
  `RESTRICT`), a minute or so.
- A discard caused by a user's deletion is now audited as the cleanup
  system actor rather than the actor of the deletion, since it happens
  after the deletion commits.
- Plan: Task 13's text is corrected in place (lock order, `CleanupStores`
  shape, test names).

## Mutation checks

Each change below was made on its own, the named tests were run through
`tools/agentic/gate.sh narrow`, and the change was reverted. Every mutation
made at least one named test fail:

| Mutation | Tests that failed |
|----------|-------------------|
| The cleanup keeps the registry row (no `DELETE FROM catalogs`) | `cleanup_removes_the_store_data_then_the_registry_row`, `a_deleted_catalogs_name_is_reserved_until_cleanup` |
| `owe_keys` writes nothing | the key cleanup tests (every store, retry, crash after commit, clearing the key) |
| `catalog_state.deleted_at` is not set | `an_edit_racing_a_delete_either_lands_first_and_blocks_it_or_fails_after` |
| The unpublished-work check is skipped | `a_delete_without_force_never_discards_work_written_during_it`, `delete_refuses_unpublished_work_unless_forced` |
| One registry filter loses `deleted_at IS NULL` (`keys.rs`) | `every_registry_filter_on_provisioned_also_filters_deleted` |
| `open_changeset` drops `FOR KEY SHARE` | `a_changeset_opened_while_its_key_is_deleted_is_refused_and_leaves_nothing` |
| A failed try does not count an attempt | `cleanup_retries_an_unreachable_store_without_blocking_others` |
| `rows_by_names` loses `deleted_at IS NULL` | `a_deleted_catalog_resolves_to_nothing_before_its_cleanup` |
| Key cleanup does not clear the key id | `a_key_cleanup_clears_the_key_from_the_stores_history` (it loops until the run is killed) |

Not mutation-checked: `catalog_audit_survives_catalog_deletion` (the
table's append-only guard also refuses a delete, so the test checks the
outcome, not which of the two held), the admin-race test (a guard on the lock
order), the starvation test (added after the run), and the directory test's
other paths.

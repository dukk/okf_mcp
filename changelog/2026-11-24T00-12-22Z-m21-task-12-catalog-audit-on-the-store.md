# M21 Task 12: catalog audit on the store

Issue #971. Migration `0042_catalog_audit.sql`. The six catalog events
(`catalog.published`, `catalog.imported`, `catalog.repaired`,
`catalog.reverted`, `catalog.changeset_discarded`, `catalog.maintenance_run`)
are written in the store's own transaction, so an entry commits or rolls
back with the change it records. The audit view reads the account database
and every store.

## Rolling-safe

The migration adds a table only. Release-2 replicas never name it and keep
writing the six events to `public.audit_entries`; the view of a release-3
replica reads both tables, so those entries appear.

## Added

- `catalog.catalog_audit_entries`: plain-value ids (`audit_people`,
  `audit_credentials`, `audit_teams`; no foreign key crosses the line),
  `(at, id)` and `teams` GIN indexes, and an append-only guard (UPDATE and
  TRUNCATE refused; DELETE refused inside the retention period, passed in
  `okf.audit_retention_days`, default 400, floor 30). The table is not named
  `audit_entries`, so a statement sent to the wrong pool fails (P1).
  `target_person_id` and `target_operator` are additions to the plan's
  columns: a discard caused by a user's deletion names that user, and the
  operator marking hides an admin from team owners as in the account table.
- `audit::resolve_actor`, `resolve_person`, `resolve_run`,
  `audit::record_catalog(conn, &ResolvedActor, &CatalogEntry, now)`, and
  `audit::prune_catalog(store, before, retention_days)` (scheduling stays
  with Task 16).
- `AuditCursor`: `(at, store, id)`, the account database as store `""`.
  Owners get it sealed (`cursor::seal`/`open`, format v2, bound to the
  audit team); admins, provisioners and the CLI get the readable
  `<microseconds>:<store>:<id>`. A bare id is refused (ids are per source).
- `store` on `AuditRecord` for admins and provisioners (absent for the
  account database and for owners).

## Changed

- Write sites resolve the actor on the account transaction they already
  hold (`lock_people`, then `resolve_actor`), then write the entry on the
  store transaction: publish, import, repair, audited edit and revert,
  discard, and the discard a user's deletion causes. Order is unchanged:
  account connection, account row locks, store connection; the store
  commits before the account transaction.
- `RunWatch` holds an account transaction (read-only, opened by
  `AuditTarget::read` before the store transaction) and writes each ended
  run's entry in the store transaction; `record_ended_run` is gone.
- `audit::list` and `export_lines` take `&Stores`: the account database and
  every store are read concurrently with the same filters and limit, merged
  by `(at, store, id)` descending. Names are read from the account database
  after the merge, so deleting a user clears their name in store entries.
  A store that cannot be read fails the page (a page that skipped it would
  hide entries for good once the cursor passed their time).
- `GET /audit`'s `next_after` and `after` are strings; the CLI's `--after`
  takes a cursor, and `audit list`'s first column is the cursor.

## Tests

`a_publish_writes_its_audit_entry_on_the_store`,
`a_failed_publish_writes_no_audit_entry`,
`entries_from_before_the_split_appear_once_in_the_view`,
`store_audit_refuses_update_and_recent_delete`,
`store_audit_prune_honours_the_passed_retention_and_its_floor`,
`audit_view_pages_across_two_stores_and_accounts_without_gaps_or_repeats`,
`audit_view_keeps_entries_after_the_last_catalog_on_a_store_is_deleted`,
`audit_view_keeps_entries_after_a_catalog_is_transferred`,
`forgetting_a_person_clears_their_name_in_store_entries_too`.

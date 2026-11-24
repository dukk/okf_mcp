# M21 issue #1023: recovery of half-made catalogs is per store

Date: 2026-11-24

## Why

`Task::ProvisionCatalogs` took the oldest 100 half-made catalogs across all
stores on every run. Once 100 of them belonged to an unreachable store, every
run took those same rows, and a later half-made catalog on a healthy store was
never recovered, against the milestone's rule that a lost store costs only its
own catalogs.

## What changed

- `catalogs::pending` takes a cursor (`created_at`, `id`) and a list of stores to
  leave out. Each query is still bounded by `PENDING_BATCH`.
- `Pending` carries `created_at`, for the cursor.
- `provision_catalogs` pages until a page is empty. A recovery that fails with
  `StoreUnavailable` puts its store on the skip list for the rest of the run,
  so the store costs one failed open, not one per row, and its rows no longer
  fill the pages.
- The task still returns the first failure, so the job is retried.

## Tests

- `half_made_catalogs_on_an_unreachable_store_do_not_starve_a_healthy_store`
  (101 older rows on an unreachable store, one newer row on `main`).
- `pending_pages_after_a_cursor_and_leaves_out_skipped_stores`.

## Plan

Task 10b carries a dated correction note.

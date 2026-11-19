# M18 Task 20: `GET /whoami` reads the caller's key row by id (issue #737)

`GET /whoami` read every key and grant (`keys::list`) and kept the caller's,
so each request cost, and briefly held, memory proportional to the number of
keys.

## Added

- `src/rows_returned.rs` (test only): the `RowsReturned` tracing layer that
  sums the `rows_returned` of `sqlx::query` events, moved out of the
  credential cache tests so other tests can use it.
- `identity::tests::whoami_reads_the_callers_key_not_every_key`: with 50,000
  other keys seeded by SQL, a warmed `whoami` returns under 50 rows.

## Changed

- `whoami` reads the caller's row with `keys::record_by_id`. The response is
  the same for keys, grants, unassigned keys and sessions; a row that is gone
  still gives no name and no user.

## Notes

- Other reads of every key, left as they are (not per-request hot paths):
  `me::own`, `me::owned`, `keys::managed`, `api::keys::list_keys` and
  `people::user_detail`. Filed as a follow-up.

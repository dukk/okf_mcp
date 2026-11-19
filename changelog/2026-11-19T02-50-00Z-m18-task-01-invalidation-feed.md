# M18 task 1: the invalidation feed (issue #679)

## Added
- `src/invalidate.rs`: the `Invalidation` enum (`catalog`, `catalog_meta`, `credential`, `user`, `team`, `settings`), its versioned JSON payload (ids and generations only), `invalidate(conn, &[..])` (bumps `admin_generation` once and sends one `pg_notify('okf_invalidate', ..)` per item in the caller's transaction, so a rollback sends nothing), and the `listen` task that applies each message.
- `App::apply` and `App::apply_reconnect`: a `catalog` message refreshes that catalog if loaded; every other kind reloads the key cache and settings; a lost connection or an unreadable payload reloads the keys and catches every catalog up.
- `src/backoff.rs`: the doubling reconnect delay, extracted from the Redis subscriber and used by both.
- Metrics `okf.invalidations.queued` (counted when queued in the transaction; a rollback discards the message but not the count), `.received`, `.unreadable` and `.lag`.
- `serve` spawns the listener on its own one-connection pool and awaits it at shutdown.

## Changed
- `shared::announce::run` uses `Backoff` instead of its inline doubling (same 1 s to 30 s).

## Notes
- No write path calls `invalidate` yet (Task 2), and no cache changed: the listener applies messages to today's caches, and the poll loop still runs.
- `bump_admin_generation` stays `pub(crate)`; the plan's file list was corrected.
- Plan corrections: `Backoff::next_delay`, the payload's `at` field in the tests, `apply` reusing `refresh_announced`/`catch_up_catalogs`.

# M18 Task 16: remove the poll loop and per-catalog series (issue #694)

## Added

- `invalidate::testing::applied(app, write)` (lib tests) and
  `invalidate::testing::messages_of(pool, write)`: run a store write, collect
  the change messages it committed through `NOTIFY`, and apply them to an app
  as its listener would (`App::apply`). This replaces `app.poll()` in lib
  tests, which run no listener. `invalidations_of` now calls `messages_of`.
- `tests/support/server.rs`'s `applied(app, write)` (integration tests): runs
  the write inside `invalidate::capturing` and applies what it queued with
  `App::apply_own_writes`, as the HTTP layer does for a request's own writes.
  The server's listener applies the same messages a moment later.
- `a_change_no_message_described_is_read_after_the_ttls` (`src/app.rs`): a
  key revoked, a team suspended and a catalog label changed by raw SQL (no
  message, as an older replica or a lost message leaves them) are each read
  again once `credential_ttl` / `catalog_ttl` has passed, with no poll, no
  reconnect and no announcement. It replaces the poll-sweep test
  `a_change_absorbed_by_a_messages_reload_is_still_swept_by_the_next_poll`.
- `poll_interval_parses_with_a_warning_and_is_ignored` (`src/config.rs`).
- `a_settings_message_counts_the_settings_load` (`src/mcp/changesets.rs`),
  replacing `a_poll_counts_the_key_reload`.

## Changed

- Removed `App::poll`, `poll_pass`, `poll_until`, the `polling` mutex, the
  `swept_generation` sweep, the `okf.poll` span, `Metrics::poll` and the
  `okf_poll_duration_seconds` histogram, `Metrics::catalog_refresh`, the
  `okf_catalog_refreshes_total` counter and `RefreshResult` (spec Decisions
  18 and 19). `serve` no longer spawns the poll loop.
- `settle` (`src/http/api/mod.rs`) is gone. Every call site now calls
  `App::bridge_to_m16()` directly, marked `// M16 bridge (spec Decision 14)`;
  the writer's read-your-writes comes from `invalidate::capturing` and
  `App::apply_own_writes`, which every HTTP route already runs (Task 8).
- `poll_interval` is `Option<Duration>`, default unset: it still parses (zero
  included) and `serve` logs `poll_interval is deprecated and ignored` when
  it is set (`Config::deprecation_warnings`).
- The Grafana example lost its three poll panels and the per-catalog refresh
  query; the "Catalogs and change propagation" row keeps the catalog
  announcements and key cache reload panels.
- `docs/operations.md`: the `poll_interval` row, the two removed metrics and
  the `okf.poll` span row. The rest of the prose that mentions the poll is
  Task 17's.
- Lib and integration tests that polled to see a change apply the write's
  change messages instead; tests whose poll followed only the creation of a
  key, user or grant drop it (those load on their first use, and unknown
  credentials are never cached); tests that change rows with raw SQL (which
  sends no message) call `App::apply_reconnect`, what a reconnect does.
- Deleted the tests of poll mechanics: `overlapping_polls_leave_the_newest_key_list`,
  `a_failed_reload_does_not_stop_the_pass_and_is_retried`,
  `a_failed_admin_generation_read_does_not_stop_the_pass`,
  `poll_until_stops_when_cancelled`, `each_poll_tick_is_its_own_trace`.

## Notes

What now keeps each cache fresh, and the bound on a lost message:

| Poll duty | Now | Lost-message bound |
|---|---|---|
| Settings reload | `Settings` message (`App::apply`), own-writes mark, `check_settings` per request, reconnect, M16 announcement | `credential_ttl` (`fresh_settings`) |
| Credential, grant and user caches (sweep) | `Credential`/`User`/`Team`/`CatalogMeta` messages, own writes, reconnect, M16 announcement | `credential_ttl` (moka TTL) |
| Labels (sweep) | the same messages, own writes, reconnect, M16 announcement | `credential_ttl` |
| Catalog directory rows (sweep) | `CatalogMeta`/`Catalog` messages, own writes, reconnect, M16 announcement and catalog subscriber resubscribe | `catalog_ttl`; absent names 5 s |
| Catalog snapshots | checked against the directory row on every use; `Catalog` message; M16 catalog announcement | `catalog_ttl` (the row's) |
| Writer's read-your-writes (`settle`'s poll) | `invalidate::capturing` + `App::apply_own_writes` on every HTTP route | none needed (in-process) |
| New writes reaching M16 replicas | `invalidate` still bumps `admin_generation` (their poll); `bridge_to_m16` at every former `settle` site and the consent flow; the command line sends the bridge payload | their `poll_interval` |
| M16 writes reaching new replicas | `apply_m16_announcement` on the `admin-generation` channel; catalog announcements | `credential_ttl` / `catalog_ttl` |

Background readers of `App::settings()` outside a request get no per-request
check: only `housekeeping::prune_audit` reads settings in the background, and
it calls `fresh_settings`.

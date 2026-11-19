# M18 Task 13: settings on invalidation, and the M16 bridge (issue #691)

## Added

- `App::fresh_settings`: the settings, checked against the database first
  when they are older than `credential_ttl` or marked stale (spec Decision 9).
  The check reads `admin_generation` (the settings version: `invalidate` bumps
  it for every administrative change, settings writes included) and loads the
  settings only if it moved. Callers that find the settings expired together
  run one check (`settings_reload` mutex, re-checked after taking it), and
  callers that waited while a check failed take its answer (`settings_rounds`)
  instead of each running another failing one, whether a TTL or a stale mark
  started it (stale marks are counted, `settings_marks`/`checked_marks`, so
  only a mark newer than the finished check gets a check of its own), so a database outage does not
  serialize every request behind one timeout after another. A failed check
  keeps serving the loaded settings, logs at `warn` and leaves them expired,
  so the next request retries. A validation time in the future (the
  clock stepped back) counts as expired.
- `SETTINGS_CHECK_TIMEOUT` (5 s): a settings check runs before the routes'
  own timeouts, so it has its own bound. A check past it (a read blocked
  behind a lock on `server_state`, an unresponsive connection) counts as
  failed: the loaded settings serve, nothing is satisfied, callers queued
  behind it share it, and the next request retries. Test
  `a_blocked_settings_check_answers_within_its_bound` (through the router).
- `App::reload_settings`: marks the settings stale and checks them at once
  (a `Settings` message, a reconnect, an M16 announcement, a poll pass). A
  mark keeps the settings stale for every caller until a check that began
  after it succeeds (`satisfied_marks`, written only on success): a request
  during an in-flight check waits for its answer, and a failed or cancelled
  check satisfies nothing.
- An HTTP middleware (`check_settings`) that calls `fresh_settings` before
  every route but `/healthz`, so what handlers read through the synchronous
  `App::settings` during a request is never older than `credential_ttl`.
- The M16 bridge (spec Decision 14), each item marked
  `M16 bridge (spec Decision 14): remove in the release after 0.23.`:
  - `shared::announce::BRIDGE_PAYLOAD` (`"notified"`) and `publish_bridge`.
    An M16 replica reads the payload as "check `admin_generation` now" (it is
    not a number, so never its loaded generation) and reloads, since
    `invalidate` still bumps the counter. A milestone 18 replica ignores it,
    because the change it stands for also went through the change feed.
  - `App::bridge_to_m16`: publishes it, detached, after an administrative
    HTTP write (`settle`) and after an OAuth grant is created.
  - The command line publishes the bridge payload instead of the empty one.
  - Every other payload on the channel comes from an M16 replica or command,
    which sends no change message, so `App::apply_m16_announcement` marks the
    credential and user caches and the catalog directory stale, checks the
    settings and reloads the labels, whatever generation it names.
- Tests: `a_settings_invalidation_reloads_the_settings`,
  `settings_are_revalidated_after_the_ttl_without_a_notification`,
  `concurrent_requests_past_the_ttl_reload_settings_once`,
  `a_check_loads_the_settings_only_when_admin_generation_moved`,
  `concurrent_requests_share_one_failed_check`,
  `concurrent_requests_share_one_failed_check_after_a_stale_mark`,
  `a_mark_during_a_check_gets_a_check_of_its_own`,
  `a_request_during_a_stale_check_waits_for_its_answer`,
  `a_cancelled_check_leaves_the_settings_stale`,
  `a_clock_stepped_back_expires_the_settings` (`src/app.rs`);
  `a_request_checks_settings_older_than_the_credential_ttl`
  (`src/http/mod.rs`); `an_m16_announcement_marks_credentials_and_settings_stale`,
  `the_bridge_payload_marks_nothing_stale`,
  `only_the_bridge_payload_is_not_an_m16_announcement`
  (`src/shared/announce.rs`); `an_m16_replicas_write_reaches_a_new_replica_through_its_announcement`
  and `an_administrative_http_write_publishes_the_m16_announcement`
  (`tests/multi_replica.rs`).

## Changed

- `App.settings` is `RwLock<LoadedSettings { settings, generation,
  validated_at }>`; `App::access_view` is `async` and reads `fresh_settings`.
- The `admin_generation` and `announced_generation` atomics are gone, and so
  are `reload_settings_now`, `reload_settings_if_changed`,
  `reload_moved_settings` and `loaded_admin_generation`. Labels are Task 12's
  `LabelLookup` (merged in): the M16 announcement, a reconnect and the poll's
  sweep mark it stale. `okf_keycache_reloads_total` counts settings loads.
- The poll pass no longer announces over Redis. It checks the settings, reads
  `admin_generation` itself and runs the
  sweep (credential caches and directory marked stale) whenever it moved since
  the last sweep, whether or not the settings check succeeded.
- `settle` still polls (Task 16 removes that) and then calls `bridge_to_m16`.
  Writers outside `/api/v1` (OAuth grant changes other than creation, sign-in
  and account paths) do not bridge: since the poll no longer announces, an M16
  replica sees them at its own poll (`poll_interval`), the bound the old
  announcing poll gave.
- `housekeeping::prune_audit` reads `fresh_settings`, since it runs outside
  any request.
- `tests/multi_replica.rs`: `a_change_still_announces_after_a_non_announcing_reload_saw_it_first`
  is removed (the poll no longer announces); the killed-subscriber test now
  revokes and announces as an M16 replica would, since a store revocation also
  reaches B through `NOTIFY`.

## Notes

- Mixed-version deploys, both directions: an M16 replica's write (no
  `NOTIFY`) reaches a new replica through the announcement; a new replica's
  write reaches an M16 replica through `admin_generation` (its poll) and the
  bridge payload (its subscriber).
- During the deploy, an M16 replica's poll also announces generations that new
  replicas' writes moved; new replicas then mark their caches stale once more
  than needed. That is the M16 cost (a full key reload per change) and ends
  with the bridge.
- Plan corrections: Task 13 (what shipped), Task 16 (the poll's settings
  check and `settle`'s poll), Task 17 (`docs/operations.md`'s description of
  the announcement channel).

# M16 Task 3: admin_generation announcements and the two-instance harness (issue #556)

## Added
- `shared::announce`: `publish` (the loaded `admin_generation`, or an empty "check now") on `<prefix>admin-generation`, and `listen`, a subscriber that reloads the key cache on every `Subscribed` (the first subscribe and each automatic resubscribe) and on every announcement of a generation other than the one loaded. It replaces a subscription that stays `Disconnected` for 10 s (the driver gives up after a few retries), and backs off from 1 s to 30 s when the subscription ends or fails, and counts `okf.redis.errors{operation="subscribe"}` on `Disconnected`.
- `App::reload_keys_now(announce)`, `App::catch_up_keys()` (a no-op without Redis) and `App::loaded_admin_generation()`.
- `App::announced_generation`, tracked apart from the loaded generation: an announcing reload publishes whenever the loaded generation differs from the last one announced, whether or not that call reloaded. A reload that loaded a change silently (`catch_up_keys`, a resubscribe) can no longer make the `settle` poll skip the announcement.
- `http::serve` spawns the subscriber beside the poller and logs a panic.
- `TestServer::start_shared` and `TestServer::sibling` (with `Sibling`), and `tests/multi_replica.rs`: key revoked (standalone and cluster mode), user deleted, team suspended, grant revoked, the non-announcing-reload-first interleaving, and a `CLIENT KILL TYPE pubsub` test. A lib test (`shared::announce::catch_up_tests`) pins the missed-announcement ordering deterministically. Every test runs with `poll_interval = 1h`.

## Changed
- `App::reload_keys_if_changed` takes `announce: bool` and is split into the reload (`reload_moved_keys`) and the announce step. The poll pass calls it with `true`.

## Notes
- Without `[redis]` nothing changes: no subscriber runs and `announce` has nothing to publish to.
- A lost announcement (Redis down while publishing) is counted and logged; the poll loop covers it, and the subscriber's reload on resubscribe covers a missed one.
- The grant test makes a real OAuth grant through the consent flow, revokes it through the store and polls A, as `settle` does after the API.

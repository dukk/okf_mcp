# Issue #591: key changes after a Redis restart (investigation)

## Finding
The hypothesis, that an announcement sent while no replica is subscribed
waits for `poll_interval`, does not hold for the current code:

- The issue was seen on M16. Since M18 the poll loop is gone
  (`poll_interval` is parsed, warned about and ignored). Key, user and
  setting changes reach every replica through the Postgres change feed,
  which a Redis restart does not touch, and the feed catches up on its own
  reconnect.
- Redis now carries catalog-generation announcements and the M16 bridge
  only. Both subscribers share `run`, which calls the handler's
  `subscribed` on every `Subscribed` event. A `Subscribed` follows the
  library's own resubscribe and also the fresh `subscribe` made after
  `RESUBSCRIBE_GRACE` or a failed attempt's backoff, so there is no
  resubscribe path without a catch-up.
- A failed CLI publish is counted (`okf.redis.errors`) and logged by
  `Redis::failed`; a failed connect logs a warning.

No production code changed.

## Added
- Test `a_publish_during_a_resubscribe_window_is_seen_once_the_subscriber_returns`:
  the first subscription is `Disconnected` and never recovers, the grace
  replaces it, the second subscribe is held until a publish on another
  replica has committed, then released. `catalog_ttl` is one hour so the
  TTL backstop cannot show the publish. B must read it with no further
  announcement. With the `Subscribed` catch-up removed from `run` the test
  fails; with it restored it passes.

## Notes
- The existing `a_subscriber_catches_up_on_a_missed_publish_when_it_subscribes`
  runs with the default `catalog_ttl`, so a TTL expiry can also satisfy it.
- Subscriber event logging is issue #592 and is not part of this change.

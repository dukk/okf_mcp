# Issue #592: log pub/sub subscribe, disconnect and resubscribe

## Changed
- `run` (`src/shared/announce.rs`), the loop both Redis subscribers share,
  now logs the subscription's life, each line carrying
  `subscriber="catalog"` or `subscriber="bridge"` (the new
  `Handler::NAME`) and nothing else a payload, key or URL could leak:
  - `info` on every `Subscribed`, with `resubscribe` (false only for the loop's
    first, across replaced subscriptions too) and `disconnected_ms` when a
    loss within that subscription preceded it;
  - `warn` when the library reports the connection lost (once per loss),
    when the grace expires and the subscription is replaced, when the
    stream ends, and when a subscribe fails.
- `docs/operations.md`: the "Subscriber log lines" bullet.

## Added
- Test `subscribe_disconnect_grace_failure_and_end_are_logged`: scripts
  five subscriptions through `run` and asserts each line's level, text and
  subscriber name, and that no `redis://` appears. With the subscribe-failure
  `warn` lowered to `debug` it fails; restored, it passes.

## Decisions
- No "subscribed" gauge. Every metric is named in the exported-family list,
  the documented metrics table and its tests; a per-subscriber 0/1 gauge
  would need all of those, and `okf_redis_errors_total{operation="subscribe"}`
  already counts losses. The log lines carry the state.
- The issue's "relies on the poll" no longer applies (M18 removed the poll);
  the intent, making an unsubscribed replica visible, is what shipped.
- A grace expiry logs its own warn and then the generic "ended" warn,
  since the same path drops the subscription.

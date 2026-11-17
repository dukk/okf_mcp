# M16 Task 1b: `[redis]` cluster mode (issue #570)

## Added
- `[redis] mode = "standalone" | "cluster"` (`config::RedisMode`), default standalone. The cluster arm uses `redis`'s `cluster-async` client, so ElastiCache Serverless (cluster mode only, TLS) works.
- `shared::RedisConnection`, an enum of `ConnectionManager` and `ClusterConnection` that implements `aio::ConnectionLike`.
- `Redis::subscribe(channel) -> Subscription`: a dedicated RESP3 connection with a push sender and automatic resubscription, yielding `SubscriptionEvent::{Subscribed, Message, Disconnected}`. Works in both modes.
- `TestRedis::start_cluster()`: a one-node `valkey/valkey:8.1-alpine` cluster (`TEST_REDIS_CLUSTER_URL` reuses a running one), and `TestRedis::cluster`.
- A paragraph on `[redis]` in `docs/operations.md`.

## Changed
- Every key and channel now carries one hash tag in both modes: `{<key_prefix>}<space>:<id>` and `{<key_prefix>}<name>`. `key_prefix` may not contain `{` or `}`.
- `Redis::connection()` returns `RedisConnection`; `Redis::client()` is removed.

## Notes
- The tagged names change every stored key, which is harmless while no code writes Redis state yet (Tasks 2 and later do).
- Tests: config (mode and braces), hash-tag sharing, cluster connect, `CROSSSLOT` on untagged keys, the connect-failure message, and subscribe/publish/`CLIENT KILL TYPE pubsub` in both modes.

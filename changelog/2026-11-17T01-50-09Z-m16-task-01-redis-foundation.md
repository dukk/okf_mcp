# M16 Task 1: the `[redis]` table, the shared Redis handle and Redis test support (issue #554)

## Added
- `[redis]` config table (`url_env`, `key_prefix`; unknown keys refused) with validation.
- `shared::Redis`: one `ConnectionManager`, key and channel names, error counting and spans; `shared::hashed`.
- `okf.redis.errors` counter (`okf_redis_errors_total`, in `ALL_FAMILIES`) and the `RedisOp` label set.
- `App::redis` and `App::load_with_redis`; `App::load_with_metrics` connects when `[redis]` is set.
- `tests/support/redis.rs` (`TestRedis`: `redis:7.4-alpine`, one random key prefix per test), `redis_config` and `redis_connect` for the integration tests, and `replica_pair` and `app_with_redis` for the lib tests.

## Changed
- `serve` refuses to start when the URL variable is unset or blank, does not parse, or Redis does not answer `PING`; the message names the variable, never the URL.
- `start_with_retry` and `start_labelled` in `tests/support/pg.rs` are generic over the container image.
- `oauth::state::code_key` delegates to `shared::hashed`.
- `docs/operations.md`: "Why one replica" now gives the in-memory state as the reason, not the publish lock.

## Notes
- Nothing uses Redis yet; without `[redis]` behaviour is unchanged.
- Dependencies added: `redis =1.8.2`, `futures-util`, `rustls` (ring provider install for `rediss://`).

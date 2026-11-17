# M16 Task 11: /healthz reports Redis (issue #564)

## Added
- `/healthz` has a `redis` field: `"ok"`, `"unavailable"`, or `null` without `[redis]`. It is probed with `PING` under `HEALTH_TIMEOUT`, concurrently with the database probe, so the route's deadline is unchanged.
- Tests: `healthz_without_redis_reports_null_and_keeps_its_status` and `healthz_reports_redis_without_changing_its_status` (Redis up, then stopped: still 200).

## Changed
- The exact-body tests (`src/http/mod.rs` and `tests/server.rs`) include `"redis": null`; the README row for `/healthz` names the field.

## Notes
- The status code still follows the database alone, so a Redis outage does not mark every replica unready (spec Decision 19).
- The body never carries the Redis URL or host.
- A failed or late `PING` is counted as `RedisOp::Get`; no new operation label was added.

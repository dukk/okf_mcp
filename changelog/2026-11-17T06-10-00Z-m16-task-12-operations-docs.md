# M16 Task 12: operations docs for several replicas (issue #565)

## Added
- `docs/operations.md`: a "Redis" section covering the config and `mode`
  (standalone, or cluster with ElastiCache Serverless as the example, TLS and
  `rediss://`), the single hash tag `{<key_prefix>}` and why, the key layout and
  TTLs, what fails closed when Redis is down, sizing, eviction policy, TLS and
  AUTH, and the caveats of running several replicas.
- The `[redis]` keys in the configuration table, the URL variable and
  `SSL_CERT_FILE` in the environment table.
- A Deployment variant with `replicas: 2` and `RollingUpdate`.
- The `/healthz` `redis` field, the `okf_redis_errors_total` metric and the
  Redis spans in the references.
- A dated note in the server design pointing at the multi-replica design.

## Changed
- "Why one replica" now names the real reasons (state held in each process's
  memory), not the publish lock, which is safe across processes.
- Single-replica statements elsewhere in the guide (metrics, team suspension
  timing, OAuth state, upgrades, sign-in limits) now say what changes with
  `[redis]`.

## Notes
- Documented from the code on `milestone/553-multi-replica` at the time of
  writing. Pending OAuth authorizations (issue #561, PR #582) were described
  from the plan and that PR's description, not from merged code.
- Two shipped behaviours differ from the plan and are documented as shipped:
  `/healthz` counts a failed `PING` as `get`, and the consent page and the
  token endpoint answer 503 on a Redis failure.

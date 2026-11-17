# M16: cluster mode added to the plan (ElastiCache Serverless)

## Added
- Spec Decision 20: `[redis] mode = "standalone" | "cluster"`, one hash tag (`{<key_prefix>}`) on every key and channel in both modes, scripts that name every key in `KEYS`, and RESP3 push pub/sub with automatic resubscription in both modes.
- Plan Task 1b (cluster mode), planning Decision 11, and a "Cluster mode" section of verified facts from a probe of a one-node `valkey/valkey:8.1-alpine` cluster.

## Changed
- The task graph: Tasks 2 and 3 now wait on Task 1b. Tasks 2, 3, 5, 12 and 13 carry dated notes on what cluster mode changes for them (Task 3's subscriber now uses `Redis::subscribe`).
- The non-goal "Redis Cluster" became "spreading okf-mcp's keys over several slots".

## Notes
- The production cache is ElastiCache Serverless for Valkey, which runs only in cluster mode and requires TLS. A non-cluster client can get `MOVED` there, and a script's keys must share a slot (`CROSSSLOT`).
- Still to check on the first deploy: RESP3 and cluster discovery against the real cache.

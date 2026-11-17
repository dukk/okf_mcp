# OKF MCP Server, Milestone 16 (multi-replica serve): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

> **Builds on `main` at `49b7f55`** (v0.20.0, PR #550), after epic #507 (connector review) merged.

**Goal:** `okf-mcp serve` can run as several replicas behind one Service. Optional Redis (`[redis]`) holds the short-lived state that today lives in one process's memory: MCP sessions and their key bindings, OAuth pending authorizations and codes, OIDC flows and passkey ceremonies. Two caches learn about changes on every instance at once: the key cache through pub/sub announcements of `admin_generation`, and the OAuth access-token cache through a revocation counter. Without `[redis]` nothing changes.

**Architecture:**
- **A new `src/shared/` module** owns everything that talks to Redis:
  - `Redis`, the handle: one `ConnectionManager`, the key prefix and the metrics;
  - `Ephemeral<V>` and `Index`, the short-lived keyed state;
  - `RedisSessions`, rmcp's `SessionStore` plus the binding scripts;
  - `announce`, the pub/sub of `admin_generation`.
- **Enum dispatch, not trait objects.** Each component that has a Redis variant is an enum with a `Memory` arm, today's code unchanged, and a `Redis` arm. So without `[redis]` the memory path is a direct call with no serialization.
- **`App` owns `Option<shared::Redis>`.** `App::load_with_redis` builds every component from it, so tests can run two `App`s (or two `TestServer`s) over one Postgres and one Redis.

**Tech Stack:** Rust 2021 with the existing axum 0.8 / sqlx 0.9 / rmcp 3.8 / tokio stack. New dependencies:
- `redis = "=1.8.2"` (`default-features = false`; `tokio-comp`, `connection-manager`, `script`, `tokio-rustls-comp`, and `cluster-async` from Task 1b);
- `futures-util = "0.3"`, for `StreamExt` on the pub/sub stream; already in the lock;
- `rustls = "0.23"` with `ring`, to install the process `CryptoProvider`;
- `chrono` gains its `serde` feature;
- `webauthn-rs` gains `danger-allow-state-serialisation`;
- dev: `testcontainers-modules` gains the `redis` feature.

**Spec:** `docs/superpowers/specs/2026-11-17-okf-multi-replica-design.md`.

---

## Conventions for every task

- **Branches and PRs.** Branch per task from `main`, one PR per task, linked to its sub-issue. Follow `AGENTS.md` (review rounds, keeping branches current, `tools/agentic/`) and `tools/agentic/brief-common.md`.
- **TDD.** Write the failing test, run it, implement, run it, commit.
- **Gates** run only through `tools/agentic/gate.sh`: `fmt`, `lint`, `test` and `dylint` before every push. While iterating, use `gate.sh narrow <cargo test args>` (one test-name filter per run).
- **Code style.** Comments explain Rust-specific constructs for a Python-expert, Rust-novice reader, as in the existing code.
  - Every public item gets a doc comment with `# Arguments`, `# Returns` and `# Errors`.
  - No `.unwrap()` outside tests.
  - New errors are `OkfError` variants already in use: `Config` at start-up, `Busy` (503) for a Redis failure after it.
  - Every new function has `#[tracing::instrument(.., skip_all)]` or a reason-bearing `uninstrumented_fn` allow (the `hyp_tracing` lint is at deny).
- **Privacy rules.** Each is a review checklist item for every task.
  - **Never log, record or put in a span:**
    - the Redis URL or its variable's value;
    - a session id, flow id, ceremony id or pending id;
    - a Redis key name (it embeds a hash, harmless, but still caller-derived);
    - a stored value.
  - Redis errors are recorded with the operation and the error's `kind()` only, through `split_event!` with the full error text in the local-only half.
  - Every client-presented id goes through `shared::hashed` before it becomes part of a key (spec Decision 7).
- **The memory path does not change.** A task that wraps a memory structure in an enum keeps that structure's code and tests as they are. The existing tests must pass unmodified, apart from `.await`s and a `Result` to unwrap.
- **Redis tests** use `tests/support/redis.rs` (Task 1).
  - Each test gets its own random `key_prefix`.
  - Two-instance tests set `poll_interval = 1h`, so a pass proves Redis carried the change, not the poll loop.
  - Every wait is bounded with `tokio::time::timeout` (`lessons.md`).
- **Changelog.** One entry per task, `changelog/<UTC ts>-m16-task-<NN>-<slug>.md`, with `## Added`, `## Changed` and `## Notes`.
  - Take the timestamp from `date -u +%Y-%m-%dT%H-%M-%SZ`.
  - Task 13 also writes the milestone changelog.
- **The code in this plan has not been compiled.** If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol. The exception is the Redis command snippets marked "(ran)" in Verified facts: they come from a probe crate that compiled and ran against Redis 7.4.
- **Placeholders only** in tests and docs: `example.com`, `redis.example.internal`, `alice`. No real hostnames, registries, names or keys.
- **Commit messages** end with the attribution line the coordinator gives.

## Verified facts the code relies on

`R` below is `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f`.

### redis 1.8.2

These were checked on 2026-11-17 by a probe crate (redis 1.8.2 with `tokio-comp`, `connection-manager`, `script`; testcontainers-modules 0.15.0 `redis`; image `redis:7.4-alpine`). Facts marked (ran) were executed.

**Versions**
- The newest stable release on crates.io is 1.8.2 (MSRV 1.88). 2.0.0-rc.3 needs Rust 1.96; the toolchain here is 1.99.1, but a release candidate is not pinned. The default features are `acl`, `streams`, `geospatial`, `script` and `num-bigint` (`R/redis-1.8.2/Cargo.toml` `[features] default`). This plan turns them off and picks what it uses.

**Connecting**
- `connection-manager` needs `aio` and pulls `arc-swap`, `backon` and `futures-channel` (`Cargo.toml` `connection-manager = [...]`).
- `ConnectionManager::new_with_config(client, ConnectionManagerConfig)` is at `R/redis-1.8.2/src/aio/connection_manager.rs:449`. The config is built from `ConnectionManagerConfig::new()` (`:119`), with `set_connection_timeout(Option<Duration>)` (`:204`), `set_response_timeout(Option<Duration>)` (`:196`) and `set_number_of_retries(usize)` (`:188`).
- `redis::Client::open(&str)` is at `src/client.rs:55`, and `Client::get_async_pubsub()` at `src/client.rs:681`.

**Errors (ran)**
- A refused connection to `redis://:s3cretpw@127.0.0.1:1/0` displays as `Connection refused (os error 111)`, with `kind() == Io`. Neither the URL nor the password appears.
- A URL that does not parse (`rediss://:s3cretpw@[bad`, `http://user:s3cretpw@example.com`) displays as `Redis URL did not parse - InvalidClientConfig`.
- After the server stops, an operation on the `ConnectionManager` returns `broken pipe` with `kind() == Io` and `is_io_error() == true`.
- The code still never prints a `RedisError`'s text in the exported half: only `kind()`. `RedisError::kind()` is at `src/errors/redis_error.rs:246`, and `is_io_error`, `is_timeout` and `is_connection_dropped` at `:320, :362, :372`.

**Commands (ran)**

`use redis::{AsyncCommands, SetOptions, SetExpiry, ExistenceCheck};`
- `con.set_options(k, v, SetOptions::default().with_expiration(SetExpiry::PX(1500)))` returns `Some("OK")`, and `pttl(k)` then gives `1500`.
- `SetOptions::default().conditional_set(ExistenceCheck::NX)...` on an existing key returns `None`.
- `get_del(k)`, typed as `Option<Vec<u8>>`, returns the value, then `None` the second time.
- `pexpire(k, ms)`, typed as `bool`, is `true` on a live key and `false` on a missing one.
- A key set with `PX(200)` reads `None` after 400 ms.
- `SetOptions::get(bool)` (`src/commands/mod.rs:3777`) adds `GET`, so `SET ... PX ... GET` returns the old value (Redis 6.2+). Only the builder was read here; Task 2's tests run it.
- `get_ex(k, Expiry::PX(ms))` exists (`src/commands/mod.rs:460`; `Expiry` at `src/types.rs:47`). This plan uses a script instead, because the binding check also refreshes a second key.
- `incr(k, 1)` is at `src/commands/mod.rs:514`, and `publish(channel, message)` at `:1648`.

**Lua compare-and-set (ran)**

```lua
local cur = redis.call('GET', KEYS[1])
if cur == false then return -1 end
if cur ~= ARGV[1] then return 0 end
local ttl = redis.call('PTTL', KEYS[1])
if ttl <= 0 then return -1 end
redis.call('SET', KEYS[1], ARGV[2], 'PX', ttl)
return 1
```

Run with `Script::new(..).key(k).arg(expected).arg(new).invoke_async(&mut con)`, typed as `i64`.
- A match returns `1`; the value changes and `PTTL` keeps counting down (`9992` of `10_000`).
- A mismatch returns `0`, and a missing key returns `-1`.
- `Script` uses `EVALSHA` and falls back to `EVAL` (`script` feature).

**Pub/sub (ran)**
- Subscribe with `client.get_async_pubsub().await?`, then `.subscribe(channel).await?` (`src/aio/pubsub.rs:518`), then `.into_on_message()` (`:598`), a `Stream<Item = Msg>` read with `futures_util::StreamExt::next`.
- `con.publish(channel, "42")`, typed as `usize`, returns the receiver count (1). `msg.get_payload::<String>()` returns `"42"`, and `msg.get_channel_name()` the channel.
- **When the server stops, the stream ends** (`next()` yields `None`). The subscriber must reconnect and treat the gap as missed messages.

**TLS**
- `tokio-rustls-comp = ["tokio-comp", "tls-rustls", "dep:tokio-rustls"]`, with rustls `default-features = false` (`Cargo.toml:151-155, 509-512`). Roots come from `rustls-native-certs`, which honours `SSL_CERT_FILE`.
- redis builds its config with `rustls::ClientConfig::builder()` (`src/connection.rs:1255`). That uses the process-default `CryptoProvider`, and **panics** when none is installed and the crate features name more than one (`R/rustls-0.23.46/src/crypto/mod.rs:243-257`).
- okf-mcp's lock builds rustls 0.23.46 with both `aws-lc-rs` and `ring` (`Cargo.lock`, the `rustls` entry). So `Redis::connect` first calls `rustls::crypto::ring::default_provider().install_default()` and ignores the `Err` that means one is already installed.

### Cluster mode (probed 2026-11-17, for Task 1b)

Production's cache is ElastiCache Serverless for Valkey, which runs only in cluster mode and requires TLS (spec Decision 20). AWS documents that a client that is not cluster-aware can get `MOVED` there, and that a Lua script's keys must hash to one slot.

**One-node Valkey cluster in a container (ran, `docker` with `valkey-cli`)**
- `docker run -d -p 127.0.0.1::6379 valkey/valkey:8.1-alpine valkey-server --cluster-enabled yes --cluster-config-file nodes.conf --appendonly no --save ""` starts Valkey 8.1.11 in cluster mode.
- After start, `CONFIG SET cluster-announce-ip 127.0.0.1`, `CONFIG SET cluster-announce-port <mapped port>` and `CLUSTER ADDSLOTSRANGE 0 16383` answer `OK`. `CLUSTER INFO` shows `cluster_state:ok` within a second, and `CLUSTER SLOTS` then names `127.0.0.1:<mapped port>` for slots 0-16383. So a cluster client on the host follows the topology to the mapped port.
- `HELLO 3` answers `proto 3`, `mode cluster`.
- `EVAL "redis.call('SET',KEYS[1],1); return redis.call('GET',KEYS[2])" 2 '{okf:}a' '{okf:}b'` runs. The same script on `okf:a` and `okf:b` fails with `CROSSSLOT Keys in request don't hash to the same slot`. That is the reason for the hash tag, and a test can use it to prove the container really is a cluster.
- The image pulled with plain `docker pull`. If testcontainers' own pull answers 401 (as for Redis, below), `docker pull valkey/valkey:8.1-alpine` first.

**redis 1.8.2 cluster API (read, not run)**
- `cluster-async = ["aio", "cluster", "dep:log", "dep:futures-channel"]` (`R/redis-1.8.2/Cargo.toml:67-72`). With `tokio-rustls-comp`, the cluster client takes TLS from a `rediss://` URL; `ClusterClientBuilder::tls(TlsMode)` is at `src/cluster_handling/client.rs:595`.
- `ClusterClientBuilder::use_protocol(ProtocolVersion)` is at `client.rs:750`, `push_sender` at `:802`, and `ClusterClient::get_async_connection()` at `:983`.
- The async `ClusterConnection` implements `aio::ConnectionLike` (`src/cluster_handling/async_connection/mod.rs:1777`), so `AsyncCommands` and `Script::invoke_async` work on it. `subscribe` (`:367`) and `ssubscribe` (`:437`) need RESP3 (`check_resp3!`). Its docs say subscriptions are resubscribed automatically after a disconnection, and the user then receives further `Subscribe` pushes.
- Standalone: `ConnectionManagerConfig::set_push_sender` (`src/aio/connection_manager.rs:234`) and `set_automatic_resubscription` (`:240`) give the same behaviour on a `ConnectionManager`.
- `PushInfo { kind: PushKind, data: Vec<Value> }` (`src/types.rs:2633`). The kinds used are `Subscribe` (`:364`), `Message`, and `Disconnection` (`:346`, sent by the library when a connection closes).

### testcontainers-modules 0.15.0
- The `redis` feature exists (`R/testcontainers-modules-0.15.0/Cargo.toml:96`). It provides `testcontainers_modules::redis::{Redis, REDIS_PORT}` (`src/redis/mod.rs`), and `REDIS_PORT` is 6379.
- **`Redis::default()` is tag `5.0`** (`src/redis/standalone.rs:3-4`), which has no `GETDEL` (Redis 6.2+). Tests use `.with_tag("7.4-alpine")`. Readiness is `WaitFor::message_on_stdout("Ready to accept connections")` (`:53-54`).
- On this machine testcontainers' own image pull (bollard) answered 401, while `docker pull redis:7.4-alpine` succeeded. A missing image is pulled by testcontainers in CI as usual; locally, `docker pull redis:7.4-alpine` first if the pull fails.

### rmcp 3.8.0
`T` = `R/rmcp-3.8.0/src/transport/streamable_http_server`.
- **The store API.** `StreamableHttpServerConfig::session_store: Option<Arc<dyn SessionStore>>` (`T/tower.rs:154`, default `None` at `:206`).
  - `SessionStore` is an `#[async_trait::async_trait]` trait (`T/session/store.rs:114-126`) with:
    - `load(&self, session_id: &str) -> Result<Option<SessionState>, SessionStoreError>`;
    - `store(&self, session_id: &str, state: &SessionState) -> Result<(), SessionStoreError>`;
    - `delete(&self, session_id: &str) -> Result<(), SessionStoreError>`.
  - `SessionStoreError = Box<dyn std::error::Error + Send + Sync + 'static>` (`:79`).
  - `SessionState { pub initialize_params: InitializeRequestParams }` is `#[non_exhaustive]`, `Serialize + Deserialize`, and built with `SessionState::new(params)` (`:65-76`).
  - `async-trait` 0.1.92 is already a dependency of okf-mcp (`Cargo.toml:40`).
- **When rmcp calls the store.**
  - `store` runs once, after a successful `initialize` handshake (`T/tower.rs:2185-2200`). A failure is logged with the session id, `warn!("Failed to persist session {} to store: {e}", session_id)` (`:2195`), and ignored.
  - `delete` runs only on `DELETE /mcp`, after the local close (`:2381-2385`). A failure is logged with the session id (`:2383`).
  - `load` runs only from `try_restore_from_store` (`:1655`), when a request names a session this instance does not hold: POST at `:1911`, GET at `:2047`. A load error is logged at `error` with `session_id` as a field (`:1706-1712`) and answers 500. `Ok(None)` answers 404 `Not Found: Session not found` (`:1913-1919`).
- **Paths.** `SessionState`, `SessionStore` and `SessionStoreError` are public at `rmcp::transport::streamable_http_server::session::store` (`T/session.rs:35`) and re-exported from `session` (`:37`).
- **Restore.** rmcp serializes concurrent restores of one id (`pending_restores`, `:1671-1692`). It allocates a local session (`LocalSessionManager::restore_session`, `T/session/local.rs:159-175`) and replays `initialize` with the stored params, then `notifications/initialized`. Both carry the current request's `Parts` and a `SessionRestoreMarker` (`:1750-1770`).
- **Nothing refreshes a stored entry on use.** A session worker that idles out (`SessionConfig::keep_alive`, `T/session/local.rs:1106-1126, 1234`) ends with `close_session` only (`T/tower.rs:1621-1643`), which does not touch the store.

### webauthn-rs 0.6.2
- `PasskeyRegistration` and `DiscoverableAuthentication` derive `Serialize`/`Deserialize` only with the `danger-allow-state-serialisation` feature (`R/webauthn-rs-0.6.2/src/interface.rs:20-24, 613-618`; feature at `Cargo.toml:57`).
- The crate's docs call it "danger" because the state must never reach the client. Here it goes to Redis, server-side.

### okf-mcp on `49b7f55`
- **`App`**
  - `App::load` (`src/app.rs:169`) calls `load_with_metrics(pool, config, clock, Metrics::global())` (`:192`). That builds `OAuth::new(&config)`, `SessionKeys::new(..)`, `Passkeys::new(&config)` and `OidcProviders::new(&config, env)` (`:222-228`).
  - `App` fields are at `:76-128`. `admin_generation: AtomicI64` is written only under `polling` (`:92-94`).
  - `poll_pass` (`:640`) holds `polling` for the whole pass and calls `reload_keys_if_changed` first (`:647`).
  - `reload_keys_if_changed` (`:742-783`) reads `catalogs::admin_generation`, returns if equal, reloads `keys` and `settings`, records `okf.keycache.reloads`, and stores the new generation.
- **Who bumps and who polls.**
  - `catalogs::bump_admin_generation` (`src/store/catalogs.rs:900`) runs inside the caller's transaction. Writers in `store/{catalogs,ceiling,keys,settings,teams,unlocks,users}.rs` and `store/oauth/{codes,grants}.rs` call it.
  - The API polls after an administrative change through `http::api::settle` (`src/http/api/mod.rs:299`), called from `catalogs.rs`, `keys.rs`, `people.rs`, `me.rs`, `settings.rs` and `unlocks.rs`.
  - The consent approval polls at `src/oauth/authorize.rs:993`.
  - CLI commands change Postgres from another process (`src/cli.rs`, `dispatch` at `:866-`).
- **Sessions.**
  - `mcp_service` (`src/http/mod.rs:268-296`) builds `LocalSessionManager` with `keep_alive = session_idle_timeout` and `sse_retry = None`.
  - `router` (`:314`) layers `bind_session` on `/mcp` after `require_key`.
  - `bind_session` (`src/http/sessions.rs:166-201`) refuses with `(404, "Not Found: Session not found")`, binds on an issued `mcp-session-id`, and releases on a successful `DELETE`.
- **OAuth.**
  - `OAuth { pending: ExpiringMap<PendingAuthorization>, codes: ExpiringMap<AuthorizationCode>, access: AccessTokens, .. }` (`src/oauth/mod.rs:73-93`), built in `OAuth::new` (`:108-158`).
  - The `pending` call sites are in `src/oauth/authorize.rs` at `:707` (insert), `:762` (update: CSRF, approval, consent nonce), `:782` (remove), `:820` (update: failed attempts), `:830` (remove), `:923` (update: accepted code), `:989` and `:1071` (remove). The code is inserted at `:1010`.
  - `codes` are used in `src/oauth/token.rs:371` (update: `used`/`replayed`) and `:433-435` (update: read `replayed` after commit).
  - The `:762` closure calls `fresh.take()` on a captured `Option` (`:767`); run twice, the second run finds `None`.
  - `AccessTokens` (`src/oauth/access.rs`): `verify` (`:153-190`), `remember` (`:101-120`, generation-guarded), and `forget`, called from `token.rs:273, 530` and `access.rs:300, 381, 409`. `verify` ends with `app.keys().find_grant(&live.grant_id)` (`:189`). The token exchange refuses a missing grant at `token.rs:411`.
- **OIDC and passkeys.**
  - OIDC: `OidcProviders { flows: ExpiringMap<Flow>, link_flows: ExpiringMap<String>, link_starts: KeyedLimiter, .. }` (`src/oidc.rs:262-275`), `store_flow` (`:390-421`), and the take at `src/http/oidc.rs:309`.
    - `Flow`, `FlowSecrets` and `LinkRequest` have hand-written redacting `Debug` (`src/oidc.rs:152-219`).
    - `LinkRequest.session_id` is the **raw** console session id (`:181-183`).
  - Passkeys: `Passkeys { webauthn, ceremonies: ExpiringMap<Ceremony>, by_session: ExpiringMap<String> }` (`src/http/passkeys.rs:80-86`), `remember` (`:396-445`), and `take_ceremony` (`:455-474`). `Ceremony`, `Registration`, `Pending` and `RegisterVia` (`:128-170`) have no `Debug`.
- **Serde.** `Access` (`src/store/keys.rs:19-27`) and `ClientKind` (`src/store/oauth/clients.rs:10-16`) derive no serde. `Ceiling = BTreeMap<String, Access>` (`src/store/ceiling.rs:22`). `ClientRecord` holds `DateTime<Utc>` fields (`clients.rs:63-80`), and okf-mcp's `chrono` has no `serde` feature (`Cargo.toml:56`).
- **Config.**
  - `Config` (`src/config.rs:574-630`) is `#[serde(default)]` without `deny_unknown_fields`; `[telemetry]` uses `deny_unknown_fields`.
  - `validate` (`:749`) chains the table checks (`:807-812`).
  - Secrets come through `mail::secret(env, name, key)` (`src/mail/mod.rs:162`), which names the variable, never the value.
- **Errors and metrics.**
  - `OkfError::Busy { reason }` maps to 503 (`src/http/mod.rs:156`).
  - `Metrics` instruments are fields of `Instruments` (`src/telemetry/metrics.rs:140-163`). Every family is listed in `Metrics::ALL_FAMILIES` (`:188`), which a test compares with what renders (`:1093-1100`).
- **Tests.**
  - Unit tests include `tests/support/pg.rs` through `#[path]` (`src/db.rs:8-11`).
  - `start_labelled` and `start_with_retry` are typed to `Postgres` (`tests/support/pg.rs:523-579`).
  - `TestServer` (`tests/support/server.rs:87-260`) starts one server on a fresh database.
  - The raw-MCP helpers `open_session`, `notify_on` and `stream_on` are private to `tests/server.rs:403-466`.
- **Docs.**
  - `docs/operations.md:18-20` blames the publish lock. Its example Deployment (`:386-392`) says `replicas: 1` with `strategy: Recreate`.
  - `examples/kubernetes/` holds only `metrics-service.yaml` and `servicemonitor.yaml`.
  - The server design says one replica at `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md:280-281, 1335`.
  - The publish mutex comment (`src/catalog.rs:177-182`) already names the advisory lock as the other half.

## Decisions taken while planning (2026-11-17)

1. **Milestone 16, from issue #551.** The design is the spec above; the maintainer accepted the split analysis on #551 and decided the points below. Labels: `epic`, `milestone-16`.
2. **Redis is optional** (`[redis]` with `url_env` and `key_prefix`). Without it, behaviour is unchanged and one replica is still required (the maintainer).
3. **Key revocation: every `admin_generation` bump is published over Redis pub/sub**, and every instance reloads its key cache at once. This covers key revoke, user delete, team suspension and grant revoke (the maintainer). This plan publishes from the reload that observes the bump (spec Decision 12), not from each of the ~35 bump sites: one place, after commit, and it also covers paths that nobody would remember to hook. The CLI publishes an empty announcement (Task 4).
4. **OAuth access-token cache: a revocation counter in Redis, checked on every lookup** (the maintainer's first option). The counter is seeded randomly whenever it is missing, so losing it cannot repeat a value an instance has seen (review of PR #552). The backstop is a 60 s cache age while Redis is on. Pub/sub alone was rejected: messages are lost while a subscriber reconnects, and a revoked token must not survive that.
5. **The 5-PR split, made finer** (the maintainer). The foundation is three tasks (1, 2, 3) and OAuth is three (6, 7, 8). The async conversion of `authorize.rs` gets its own task (8), and the CLI announcement too (4).
6. **CAS `update` for `pending` and `codes`**, keeping `used`/`replayed`. **Per-actor caps as index keys.** **The idle TTL refreshed in `bind_session`** (the maintainer; spec Decisions 5, 8 and 11).
7. **Passkeys and OIDC link flows store only a hash of the console session id** (the maintainer, 2026-11-17). Redis never holds the raw console session id. A registration or a link flow started from a console session finishes only if the finishing request's own session cookie hashes to the stored value. It then uses that request's own session id: to keep it when the others end, or to identify it to the store. Tasks 9 and 10.
8. **`/healthz` reports Redis without changing its status code** (the maintainer, 2026-11-17). The body gains a `redis` field: `"ok"`, `"unavailable"`, or `null` without `[redis]`. The status still follows the database alone, so a Redis outage does not mark every replica unready. It gets its own task, Task 11, since it is code with its own tests and the docs task is documentation only.
9. **Every CLI command announces, as planned** (the maintainer, 2026-11-17), read-only ones included. Each instance then reads `admin_generation` once and reloads only if it moved. A list of mutating commands was rejected: it goes stale when commands are added. Task 4.
10. **The plan PR is merged by the coordinator** after review. The epic and the sub-issues are filed after that, with the maintainer's acceptance.
11. **Cluster mode for ElastiCache Serverless** (the maintainer, 2026-11-17, after Task 1 merged). Production's cache is ElastiCache Serverless for Valkey, cluster mode only, TLS required. `[redis] mode = "standalone" | "cluster"`, one hash tag on every key and channel in both modes, and RESP3 push pub/sub in both modes (spec Decision 20). Supporting only cluster mode was rejected: plain standalone Redis and Valkey should keep working. Task 1b, which Tasks 2 and 3 now wait on.

## Simplifications and deviations relative to the issue

The spec already reflects each of these.
- **No `touch` operation.** The issue's trait lists `touch`. Only MCP bindings slide on use, and their refresh is part of the admit script (Task 5). The per-actor indexes slide inside `Index::replace`. Nothing else needs it.
- **Codes use `update`, not `take`.** `GETDEL` would drop the used code, and with it the replay detection that revokes the token family.
- **Global caps are dropped in Redis**, kept in memory (spec Decision 8).
- **Session bindings are not deleted on revocation.** The pub/sub reload makes `require_key` refuse the key on every instance (spec Decision 12). Deleting bindings per key would need a key-to-sessions index kept in step with every revocation path.
- **Raw console session ids leave Redis values.** `LinkRequest.session_id` (Task 9) and the passkey `RegisterVia::Session.session_id` (Task 10) become the session's hash. The finish matches the finishing request's cookie against it (Decision 7).
- **No Deployment under `examples/kubernetes/`** exists. The issue's "`examples/kubernetes`" Deployment is the one in `docs/operations.md`, which Task 12 changes.
- **`src/catalog.rs:177`'s comment is already right** and is not changed.

## Scope

| Task | Delivers |
|---|---|
| 1 | `[redis]` config and validation; `shared::Redis` (connect, start-up checks, key names, error counting, spans); `okf.redis.errors`; `App::redis`, `App::load_with_redis`; `tests/support/redis.rs`; the lib two-`App` helper; "Why one replica" corrected |
| 1b | `[redis] mode` (standalone or cluster); `shared::RedisConnection`; the `{<key_prefix>}` hash tag on every key and channel; `Redis::subscribe` (RESP3 pushes, automatic resubscription) and `Subscription`; `TestRedis::start_cluster` (one-node Valkey cluster) |
| 2 | `shared::ephemeral`: `Ephemeral<V>` and `Index`, memory and Redis, the versioned envelope, the CAS script; the `lessons.md` rule on re-run closures |
| 3 | `admin_generation` announcements: publish after a reload, the subscriber task, `App::reload_keys_now`, `App::catch_up_keys`; `TestServer::start_shared` / `sibling`; two-instance revocation tests |
| 4 | The CLI announcement after every successful command |
| 5 | MCP sessions: `RedisSessions` (`SessionStore`), Redis bindings with the admit/bind/release scripts, `SessionBindings`; the raw-MCP test helpers moved to `tests/support/mcp_raw.rs`; two-instance session tests |
| 6 | The access-token revocation counter and 60 s backstop; grant catch-up in exchange, refresh and verify |
| 7 | OAuth authorization codes on `Ephemeral` (CAS, replay detection kept); two-instance code tests |
| 8 | OAuth pending authorizations on `Ephemeral` (the `authorize.rs` async conversion, idempotent closures); serde on `PendingAuthorization` and what it holds; the two-instance consent test |
| 9 | OIDC flows and link flows on `Ephemeral`/`Index`; the hashed link session id; the two-instance callback test |
| 10 | Passkey ceremonies and `by_session` on `Ephemeral`/`Index`; webauthn state serialization; two-instance registration and sign-in tests |
| 11 | `/healthz` reports Redis (`"redis": "ok" \| "unavailable" \| null`) without changing its status code |
| 12 | `docs/operations.md` (why one replica, Redis, multi-replica caveats, the Deployment, the `/healthz` field), the server design note, the #357 note |
| 13 | The two-process smoke test, README, milestone changelog |

**Deliberately deferred**

| Item | Where it goes |
|---|---|
| Shared rate limits (per-address, per-email, OIDC `link_starts`, OAuth buckets) | #357; Task 12 comments there that `shared::Redis` is the place |
| The mail worker with N replicas | #458 |
| Pub/sub invalidation of catalog snapshots (read lag up to `poll_interval`) | not planned; documented |
| SSE resumption (`EventStore`) | not planned |
| Redis Sentinel; spreading okf-mcp's keys over several cluster slots | not planned; documented (cluster mode itself is Task 1b) |

## File map

| File | Tasks |
|---|---|
| `Cargo.toml`, `Cargo.lock` | 1 (`redis`, `futures-util`, `rustls`, dev `testcontainers-modules` `redis`), 8 (`chrono` `serde`), 10 (`webauthn-rs` feature) |
| `src/lib.rs` | 1 (`pub mod shared;`) |
| `src/shared/mod.rs` | 1 (create), 1b (`RedisConnection`, hash-tagged names, `subscribe`), 2 (`pub mod ephemeral`), 3 (`pub mod announce`), 5 (`pub mod sessions`) |
| `src/shared/ephemeral.rs` | 2 (create) |
| `src/shared/announce.rs` | 3 (create), 4 |
| `src/shared/sessions.rs` | 5 (create) |
| `src/config.rs` | 1 (`RedisConfig`, validation), 1b (`RedisMode`, no braces in `key_prefix`) |
| `src/telemetry/metrics.rs` | 1 (`okf.redis.errors`, `RedisOp`) |
| `src/app.rs` | 1 (`redis` field, `load_with_redis`), 3 (`reload_keys_now`, `catch_up_keys`, announce flag), 5 (`sessions` type), 6 (`OAuth::new` arg), 9 (`OidcProviders::new` arg), 10 (`Passkeys::new` arg) |
| `src/cli.rs` | 1 (crypto provider install in `serve`), 4 (announce in `dispatch`) |
| `src/http/mod.rs` | 3 (spawn the subscriber in `serve`), 5 (`mcp_service` session store), 11 (`/healthz`) |
| `src/http/sessions.rs` | 5 |
| `src/oauth/mod.rs` | 6 (`AccessTokens::new(redis)`, `OAuth::new(config, redis)`), 7 (`codes`), 8 (`pending`) |
| `src/oauth/access.rs` | 6 |
| `src/oauth/token.rs` | 6 (catch-up, async `forget`), 7 |
| `src/oauth/authorize.rs` | 7 (the code insert at `:1010`), 8 |
| `src/oauth/state.rs` | 1 (`code_key` delegates to `shared::hashed`), 7 (serde on `AuthorizationCode`), 8 (serde on `PendingAuthorization`, `AcceptedCode`, `UserConsent`) |
| `src/oauth/expiring.rs` | 2 (module docs only) |
| `src/store/keys.rs`, `src/store/oauth/clients.rs` | 8 (serde on `Access`, `ClientKind`, `ClientRecord`) |
| `src/oidc.rs`, `src/http/oidc.rs` | 9 |
| `src/http/passkeys.rs` | 10 |
| `src/mcp/testing.rs` | 1 (`TestApp::sibling`, `app_with_redis`) |
| `tests/support/pg.rs` | 1 (generic `start_labelled`/`start_with_retry`) |
| `tests/support/redis.rs`, `tests/support/mod.rs` | 1 (create), 1b (`start_cluster`) |
| `tests/support/server.rs` | 3 (`start_shared`, `sibling`) |
| `tests/support/mcp_raw.rs` | 5 (create, moved from `tests/server.rs`) |
| `tests/server.rs` | 5 (use the moved helpers) |
| `tests/multi_replica.rs` | 3 (create), 5 |
| `tools/agentic/lessons.md` | 2 |
| `docs/operations.md` | 1 ("Why one replica" correction), 12 |
| `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` | 12 |
| `docs/superpowers/specs/2026-11-17-okf-multi-replica-design.md` | any task that corrects it |
| `README.md`, `changelog/*-m16-milestone.md` | 13 |
| `changelog/` | every task |

## Task dependencies

Each line is `task (the tasks it needs)`. A task starts when every task in its parentheses has merged. Tasks on one line can run in parallel.

```
wave 1:  1 ()
wave 2:  1b (1)       11 (1)
wave 3:  2 (1b)       3 (1b)
wave 4:  4 (3)   5 (2, 3)   6 (3)   9 (2)   10 (2)
wave 5:  7 (2, 6)
wave 6:  8 (7)
wave 7:  12 (1b, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11)
wave 8:  13 (all)
```

> **Corrected (2026-11-17, cluster mode).** Task 1b was added after Task 1 merged. Every task that talks to Redis after Task 1 builds on its `RedisConnection`, its key names and its `subscribe`, so 2 and 3 wait on it (and everything after them through them). 11 only `PING`s and keeps its edge to 1; if 1b merges first, 11 merges it and uses `RedisConnection`.

Why each edge, and the shared files to merge with care:
- **Everything needs 1** (`shared::Redis`, `RedisConfig`, `App::load_with_redis`, the Redis test support).
- **2 and 3 are independent.** 2 is `shared/ephemeral.rs` alone, and 3 is `shared/announce.rs`, `app.rs` and `http/mod.rs`. Both add a `pub mod` line to `src/shared/mod.rs`: keep both.
- **4, 5 and 6 need 3.**
  - 4 extends `announce.rs`.
  - 5's revocation test needs the announcements, and its test harness is 3's `TestServer::sibling`. **5 also needs 2:** `RedisSessions` stores its state in Task 2's `Envelope`, which Task 5 makes `pub(crate)`.
  - 6's catch-up is `App::catch_up_keys` (3).
  - 5 and 3 both edit `src/http/mod.rs`, and 5 and 6 both edit `src/app.rs` (different lines): keep both.
- **9 and 10 need 2** (`Ephemeral`, `Index`). They are lib-level two-`App` tests (Task 1's helper), so they don't need 3. 9 and 10 both change one constructor line in `src/app.rs`: keep both.
- **7 needs 2 and 6.** It needs `Ephemeral`, and its cross-instance exchange relies on 6's grant catch-up. 6 and 7 both edit `src/oauth/mod.rs` and `token.rs`.
- **8 needs 7.** Both edit `src/oauth/authorize.rs` and `src/oauth/state.rs`, and 8's consent test exchanges a code across instances.
- **11 needs only 1** (`App::redis`). It edits `healthz` in `src/http/mod.rs`, which 3 and 5 also edit (`serve`, `mcp_service`): different functions, so keep both sides.
- **12 needs every feature task,** so the docs describe what shipped. **13 is last.**

---

### Task 1: The `[redis]` table, the `shared::Redis` handle, and Redis test support

**Files:**
- Create: `src/shared/mod.rs`, `tests/support/redis.rs`
- Modify:
  - `Cargo.toml`, `src/lib.rs`, `src/config.rs`, `src/telemetry/metrics.rs`, `src/app.rs`, `src/cli.rs`;
  - `src/oauth/state.rs` (`code_key`), `src/mcp/testing.rs`;
  - `tests/support/pg.rs`, `tests/support/mod.rs`;
  - `docs/operations.md:18-20`.

- [ ] **Step 1: Dependencies**

```toml
# Optional shared state for multi-replica serve (src/shared, spec
# 2026-11-17-okf-multi-replica-design.md). Pinned exactly: 1.8.2 is the newest
# stable release (2.0 is a release candidate). Default features off; we use the
# tokio connection manager, Lua scripts and rustls TLS (`rediss://`).
redis = { version = "=1.8.2", default-features = false, features = ["tokio-comp", "connection-manager", "script", "tokio-rustls-comp"] }
# `StreamExt::next` on the pub/sub message stream (already in the lock via redis).
futures-util = { version = "0.3", default-features = false }
# Only to install ring as the process CryptoProvider before a `rediss://`
# connect: the lock builds rustls with both ring and aws-lc-rs, and rustls then
# refuses to guess (rustls 0.23 `crypto/mod.rs`).
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }
```

and in `[dev-dependencies]`: `testcontainers-modules = { version = "0.15.0", features = ["postgres", "redis"] }`. Run `cargo tree -d -e normal | grep -E "^(redis|rustls|tokio-rustls) "`. Expect one version of each.

- [ ] **Step 2: Write the failing config tests** (`src/config.rs` tests)

```rust
#[test]
fn redis_is_off_by_default_and_parses_with_defaults() {
    assert_eq!(Config::default().redis, None);
    let config = Config::from_toml("[redis]\nurl_env = \"OKF_REDIS_URL\"\n").unwrap();
    let redis = config.redis.expect("a [redis] table");
    assert_eq!(redis.url_env, "OKF_REDIS_URL");
    assert_eq!(redis.key_prefix, "okf:");
    config.validate().unwrap();
}

#[test]
fn redis_rejects_unknown_keys_and_bad_values() {
    assert!(Config::from_toml("[redis]\nurl_env = \"X\"\nurl = \"redis://h\"\n").is_err());
    for (toml, needle) in [
        ("[redis]\nurl_env = \"\"\n", "redis.url_env"),
        ("[redis]\nurl_env = \"HAS SPACE\"\n", "redis.url_env"),
        ("[redis]\nurl_env = \"X\"\nkey_prefix = \"\"\n", "redis.key_prefix"),
        ("[redis]\nurl_env = \"X\"\nkey_prefix = \"a b\"\n", "redis.key_prefix"),
    ] {
        let message = Config::from_toml(toml).unwrap().validate().unwrap_err();
        assert!(message.contains(needle), "{toml}: {message}");
    }
}
```

Run `tools/agentic/gate.sh narrow --lib -- config::tests::redis`. Expect a compile failure: there is no field `redis`.

- [ ] **Step 3: `RedisConfig`**

```rust
/// The `[redis]` table (spec Decision 1). Absent: every short-lived state stays
/// in memory and one replica is required. Present: `serve` keeps it in Redis.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedisConfig {
    /// The environment variable holding the Redis URL (`redis://` or
    /// `rediss://`; it may carry a password, so it never goes in the file).
    pub url_env: String,
    /// Prepended to every key and channel, so one Redis can serve several
    /// deployments. Default `"okf:"`.
    #[serde(default = "default_key_prefix")]
    pub key_prefix: String,
}
```

- Add `pub redis: Option<RedisConfig>,` to `Config`; the `Default` impl gives `None`.
- `default_key_prefix()` returns `"okf:".to_string()`.
- `RedisConfig::validate(&self) -> Result<(), String>` refuses:
  - an `url_env` that is empty or has a character outside `[A-Za-z0-9_]`;
  - a `key_prefix` that is empty, over 64 bytes, or holds whitespace or a control character.
- Chain `if let Some(redis) = &self.redis { redis.validate()?; }` in `Config::validate` before `self.validate_oidc()?`.

- [ ] **Step 4: The metric** (`src/telemetry/metrics.rs`)

```rust
/// What a Redis call was doing, the `operation` label of `okf.redis.errors`
/// (a closed set: spec Decision 17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedisOp {
    Connect, Get, Set, Take, Update, Remove, Index, Admit, Bind, Publish, Subscribe, Counter, Decode,
}
```

- `RedisOp::as_str` gives the lowercase names.
- Add the instrument `redis_errors: Counter<u64>`, built from `meter.u64_counter("okf.redis.errors").with_description("Failed Redis calls, by operation.")`, and `pub fn redis_error(&self, op: RedisOp)`.
- Add `"okf_redis_errors_total"` to `ALL_FAMILIES`.
- The family-render test at `:1093` records one event of every kind: add a `metrics.redis_error(RedisOp::Get)` there.

- [ ] **Step 5: `src/shared/mod.rs`**

```rust
//! Optional shared state for running several `serve` replicas (spec
//! `2026-11-17-okf-multi-replica-design.md`): one Redis connection and the
//! key names every component uses. Nothing here runs without `[redis]`.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use redis::aio::{ConnectionManager, ConnectionManagerConfig};

use crate::config::RedisConfig;
use crate::error::OkfError;
use crate::telemetry::metrics::{Metrics, RedisOp};
use crate::telemetry::subscriber::split_event;

/// How long a connection attempt may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long one command may take before it counts as failed.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);

/// The instance's Redis: a multiplexed connection that reconnects by itself,
/// the client (for the pub/sub connection), the key prefix and the metrics.
/// Cheap to clone (every field is a handle).
#[derive(Clone)]
pub struct Redis {
    client: redis::Client,
    connection: ConnectionManager,
    prefix: Arc<str>,
    metrics: Metrics,
}

// Hand-written: the client holds the URL, which may carry a password.
impl std::fmt::Debug for Redis {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Redis").field("prefix", &self.prefix).finish_non_exhaustive()
    }
}

impl Redis {
    /// Connects and checks the server answers, for `serve` start-up.
    ///
    /// # Arguments
    /// * `config` - the `[redis]` table (validated).
    /// * `env` - reads an environment variable (injectable for tests).
    /// * `metrics` - where `okf.redis.errors` is counted.
    ///
    /// # Returns
    /// The handle.
    ///
    /// # Errors
    /// [`OkfError::Config`] (path `[redis]`) when the variable is unset or
    /// blank, its value is not a Redis URL, or Redis does not answer `PING`
    /// within [`CONNECT_TIMEOUT`]. The message names the variable and the
    /// error's kind, never the URL.
    #[tracing::instrument(level = "info", skip_all, fields(db.system.name = "redis", db.operation.name = "connect"))]
    pub async fn connect(
        config: &RedisConfig,
        env: &dyn Fn(&str) -> Option<String>,
        metrics: &Metrics,
    ) -> Result<Self, OkfError> {
        // `rediss://` builds a rustls config from the process default; with
        // two providers compiled in, rustls would panic without this. `Err`
        // only means one is installed already.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let name = &config.url_env;
        let fail = |message: String| OkfError::Config { path: "[redis]".to_string(), message };
        let url = env(name)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| fail(format!("redis.url_env names {name}, which is not set or is empty")))?;
        let client = redis::Client::open(url.as_str())
            .map_err(|_| fail(format!("{name} does not hold a redis:// or rediss:// URL")))?;
        let manager_config = ConnectionManagerConfig::new()
            .set_connection_timeout(Some(CONNECT_TIMEOUT))
            .set_response_timeout(Some(RESPONSE_TIMEOUT))
            .set_number_of_retries(1);
        let unreachable = |error: redis::RedisError| {
            metrics.redis_error(RedisOp::Connect);
            fail(format!("the Redis {name} names did not answer ({:?})", error.kind()))
        };
        let mut connection = ConnectionManager::new_with_config(client.clone(), manager_config)
            .await
            .map_err(unreachable)?;
        let _: String = redis::cmd("PING").query_async(&mut connection).await.map_err(unreachable)?;
        Ok(Self { client, connection, prefix: Arc::from(config.key_prefix.as_str()), metrics: metrics.clone() })
    }

    /// The full key of `id` in `space` (`<prefix><space>:<id>`).
    ///
    /// # Arguments
    /// * `space` - the key space (`"oauth:code"`, ...).
    /// * `id` - already hashed where it came from a client ([`hashed`]).
    ///
    /// # Returns
    /// The key.
    pub fn key(&self, space: &str, id: &str) -> String {
        format!("{}{space}:{id}", self.prefix)
    }

    /// The pub/sub channel `name` under the prefix.
    ///
    /// # Arguments
    /// * `name` - the channel's own name.
    ///
    /// # Returns
    /// The channel.
    pub fn channel(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }

    /// A handle on the shared connection, for one call (a clone is cheap:
    /// every clone multiplexes the same socket).
    ///
    /// # Returns
    /// The connection.
    pub fn connection(&self) -> ConnectionManager {
        self.connection.clone()
    }

    /// The client, for a dedicated pub/sub connection.
    ///
    /// # Returns
    /// The client.
    pub fn client(&self) -> &redis::Client {
        &self.client
    }

    /// Counts and logs a failed call, and gives the error a request answers
    /// with: 503, retryable.
    ///
    /// # Arguments
    /// * `op` - what the call was doing.
    /// * `error` - the driver's error (its text stays local).
    ///
    /// # Returns
    /// [`OkfError::Busy`].
    pub fn failed(&self, op: RedisOp, error: &redis::RedisError) -> OkfError {
        self.metrics.redis_error(op);
        split_event!(
            error,
            { operation = op.as_str(), kind = ?error.kind(), },
            { %error, },
            "a Redis call failed"
        );
        OkfError::Busy { reason: "the shared session store is unavailable".to_string() }
    }

    /// Counts a failure that has no driver error (a pub/sub stream that
    /// ended).
    ///
    /// # Arguments
    /// * `op` - what was failing.
    pub fn count(&self, op: RedisOp) {
        self.metrics.redis_error(op);
    }

    /// Counts a value that would not decode (another version, or corrupt).
    ///
    /// # Arguments
    /// * `space` - its key space (a closed set, safe to log).
    pub fn undecodable(&self, space: &'static str) {
        self.metrics.redis_error(RedisOp::Decode);
        tracing::warn!(space, "a stored value did not decode; treated as absent");
    }
}

/// The hex SHA-256 of a client-presented id, the only form an id takes in a
/// key name (spec Decision 7).
///
/// # Arguments
/// * `id` - the id as presented.
///
/// # Returns
/// 64 lowercase hex characters.
pub fn hashed(id: &str) -> String {
    // `fold` with `write!` appends each byte's two hex digits to one
    // preallocated `String` (Python: `hashlib.sha256(...).hexdigest()`).
    crate::keys::hash(id).iter().fold(String::with_capacity(64), |mut hex, byte| {
        // Writing to a `String` cannot fail.
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}
```

Mark the trivial accessors (`key`, `channel`, `connection`, `client`, `Debug::fmt`) with the usual `uninstrumented_fn` allows. `failed`, `count` and `undecodable` get `#[tracing::instrument(level = "trace", skip_all)]`. `oauth::state::code_key` becomes `crate::shared::hashed(code)`, and its FIPS test still passes. Add `pub mod shared;` to `src/lib.rs`.

- [ ] **Step 6: `App`**

- Add the field `redis: Option<shared::Redis>`, its accessor `pub fn redis(&self) -> Option<&shared::Redis>`, and:

```rust
/// [`App::load_with_metrics`] with the Redis handle given (or none): tests
/// pass one handle to two apps, as two replicas share one Redis.
///
/// # Arguments
/// * `pool`, `config`, `clock`, `metrics` - as for `load_with_metrics`.
/// * `redis` - the shared Redis; `None` keeps everything in memory.
///
/// # Returns
/// The shared state.
///
/// # Errors
/// As [`App::load`].
#[tracing::instrument(level = "debug", skip_all)]
pub async fn load_with_redis(
    pool: PgPool, config: Config, clock: Arc<dyn Clock>, metrics: Metrics, redis: Option<shared::Redis>,
) -> Result<Arc<Self>, OkfError>
```

- `load_with_metrics` connects when `config.redis` is set, then delegates:
  `let redis = match &config.redis { Some(table) => Some(shared::Redis::connect(table, &|name| std::env::var(name).ok(), &metrics).await?), None => None };`
  The body of today's `load_with_metrics` moves into `load_with_redis`, which stores `redis` in the struct. Later tasks pass `redis.as_ref()` to the constructors.

> **Corrected (issue #554).** `load_with_metrics` delegates with `Box::pin(Self::load_with_redis(..)).await`, not a bare `.await`: the extra un-boxed async level pushed `tests/investigator.rs`'s `acceptance::server()` future past rustc's layout recursion limit (`queries overflow the depth limit`). `src/cli.rs` needed no change, as Step 10 says. Task 1 also adds three lib tests to `src/shared/mod.rs` (two replicas share one Redis and `app_with_redis` has one; `App::load` refuses an unset `[redis]` variable) so `replica_pair` and `app_with_redis` are used, as the `dead_code` lint requires.
- `App::load`'s `# Errors` doc gains the `[redis]` failures.

- [ ] **Step 7: Start-up tests** (`src/shared/mod.rs` tests; `Redis::connect` with an injected `env`)

```rust
#[tokio::test]
async fn connect_refuses_an_unset_or_blank_variable_and_names_only_it() {
    let config = RedisConfig { url_env: "OKF_TEST_REDIS_URL".into(), key_prefix: "okf:".into() };
    for value in [None, Some("  ")] {
        let error = Redis::connect(&config, &|_| value.map(str::to_string), &Metrics::global()).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("OKF_TEST_REDIS_URL"), "{message}");
    }
}

#[tokio::test]
async fn connect_refuses_an_unreachable_or_malformed_url_without_printing_it() {
    let config = RedisConfig { url_env: "OKF_TEST_REDIS_URL".into(), key_prefix: "okf:".into() };
    for url in ["redis://:s3cretpw@127.0.0.1:1/0", "http://user:s3cretpw@example.com"] {
        let error = Redis::connect(&config, &|_| Some(url.to_string()), &Metrics::global()).await.unwrap_err();
        let message = error.to_string();
        assert!(!message.contains("s3cretpw") && !message.contains("127.0.0.1") && !message.contains("example.com"), "{message}");
        assert!(message.contains("OKF_TEST_REDIS_URL"), "{message}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_answers_a_live_redis() {
    let server = crate::db::redis_support::TestRedis::start().await;
    let redis = testing::connect(&server).await;
    assert!(redis.key("oauth:code", "abc").ends_with("oauth:code:abc"));
}

#[test]
fn hashed_is_the_hex_sha256() {
    assert_eq!(hashed("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
}
```

`Metrics::global()` (`src/telemetry/metrics.rs:468`) is the global provider's meter, a no-op in unit tests. A test that reads a counter builds `Metrics::new(&meter)` on a local provider with a Prometheus registry, as `oauth::testing::oauth_app_with_metrics` does.

- [ ] **Step 8: `tests/support/redis.rs`**

The file names nothing from the crate, so both builds can include it, as `pg.rs` is. Its only sibling is `pg`, for the container helpers:
- The integration tests declare `pub mod redis;` in `tests/support/mod.rs`, next to `pub mod pg;`.
- The unit tests declare it in `src/db.rs`, next to `pg_support`, with an alias so `super::pg` resolves:

```rust
#[cfg(test)]
pub(crate) use pg_support as pg;
#[cfg(test)]
#[path = "../tests/support/redis.rs"]
pub(crate) mod redis_support;
```

```rust
//! A Redis for tests: the shared server `TEST_REDIS_URL` names, else a
//! `redis:7.4-alpine` container per [`TestRedis`] (testcontainers-modules'
//! default tag, 5.0, lacks GETDEL). Each [`TestRedis`] has its own random key
//! prefix, so tests sharing one server never see each other's keys.

// Each crate that includes this file uses a different part of it.
#![allow(dead_code)]

use testcontainers_modules::redis::{Redis as RedisImage, REDIS_PORT};
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

/// The variable naming a running Redis every test may share.
pub const SERVER_URL_VAR: &str = "TEST_REDIS_URL";
/// The variable a test's `[redis] url_env` names; [`TestRedis::env`] answers it.
pub const URL_VAR: &str = "OKF_TEST_REDIS_URL";

/// One test's Redis: its URL, its key prefix, and the container if it
/// started one (removed when this is dropped).
pub struct TestRedis {
    /// `redis://127.0.0.1:<port>/0`.
    pub url: String,
    /// `okftest:<pid>-<n>:`, unique to this value.
    pub prefix: String,
    container: Option<ContainerAsync<RedisImage>>,
}

impl TestRedis {
    /// Uses `TEST_REDIS_URL`, or starts a labelled container, retried once
    /// (`pg::start_with_retry`).
    pub async fn start() -> Self {
        let prefix = format!("okftest:{}:", super::pg::unique_start_label());
        if let Some(url) = std::env::var(SERVER_URL_VAR).ok().filter(|url| !url.is_empty()) {
            return Self { url, prefix, container: None };
        }
        let container =
            super::pg::start_with_retry(|| RedisImage::default().with_tag("7.4-alpine")).await;
        let port = container.get_host_port_ipv4(REDIS_PORT).await.expect("the redis port");
        Self { url: format!("redis://127.0.0.1:{port}/0"), prefix, container: Some(container) }
    }

    /// An `env` reader for `Redis::connect` that knows only [`URL_VAR`].
    pub fn env(&self) -> impl Fn(&str) -> Option<String> + '_ {
        move |name| (name == URL_VAR).then(|| self.url.clone())
    }

    /// Stops the container (outage tests); `None` on a shared server, where
    /// a test cannot stop Redis for everyone and should skip itself.
    pub async fn stop(&self) -> Option<()> {
        self.container.as_ref()?.stop().await.expect("stop the redis container");
        Some(())
    }

    /// Starts a stopped container again on the same host port.
    pub async fn start_again(&self) -> Option<()> {
        self.container.as_ref()?.start().await.expect("start the redis container");
        Some(())
    }
}
```

- Outage tests skip themselves (return early with an `eprintln!` note) when `stop()` gives `None`.
- If testcontainers maps a new host port on `start()`, start that test's container with `.with_mapped_port(free_port, REDIS_PORT.tcp())` instead, and add a `TestRedis::start_fixed_port()`. Then fix this plan.

The crate-side helpers are written once per build:
- **Lib.** `src/shared/mod.rs` gets a `#[cfg(test)] pub(crate) mod testing` with:
  - `config(&TestRedis) -> RedisConfig { url_env: URL_VAR.into(), key_prefix: server.prefix.clone() }`;
  - `connect(&TestRedis) -> Redis`, which is `Redis::connect(&config(server), &server.env(), &Metrics::global())` with `.expect("connect to the test redis")`.
- **Integration.** `tests/support/server.rs` gets the same two functions over `okf_mcp::`, as `redis_config` and `redis_connect`.

`start_with_retry` and `start_labelled` in `pg.rs` become generic over `I: testcontainers::Image`. The panic message names the image (`request().image().name()`). Their Postgres callers don't change.

- [ ] **Step 9: The lib two-`App` helper** (`src/mcp/testing.rs`)

```rust
/// Two apps over one database and one Redis, as two replicas: `app_with`'s
/// catalogs, `config` with `[redis]` and `poll_interval = 1h` (so a test
/// that passes was carried by Redis, not by a poll).
pub(crate) async fn replica_pair(
    catalogs_and_files: &[(&str, &[(&str, &str)])],
    config: Config,
) -> (TestApp, Arc<App>, crate::db::redis_support::TestRedis)
```

The first `TestApp` owns the database. The second `App` gets its own pool on `db.url`, through `App::load_with_redis(.., Some(redis.connect(..)))` for each. Also add `app_with_redis(config: Config) -> (TestApp, TestRedis)`, a single app with Redis (its `TestRedis` returned for the test to hold, or to stop).

- [ ] **Step 10: `serve` and the docs correction**

`src/cli.rs` `serve` needs no change: `App::load` connects. Check that a `[redis]` failure surfaces as `load the catalogs: [redis]: ...`.

In `docs/operations.md:18-20`, replace the publish-lock sentence: "**Why one replica:** short-lived state lives in the process's memory: MCP sessions and their key bindings, OAuth consent pages and codes, OIDC and passkey sign-ins in progress. A request that lands on another replica would not find it. Publishing is safe across processes (a Postgres advisory lock orders publishes of a catalog). Milestone 16 adds `[redis]` to share that state." Task 12 rewrites the section fully.

- [ ] **Step 11: Gates, changelog, commit**

`fmt`, `lint`, `test`, `dylint`. Commit `feat: [redis] config and the shared Redis handle (M16 Task 1)`.

---

### Task 1b: Cluster mode (ElastiCache Serverless): `[redis] mode`, hash-tagged names, RESP3 pub/sub

Added 2026-11-17 after Task 1 merged (Decision 11 above, spec Decision 20). Builds on Task 1's `shared::Redis`.

**Files:**
- Modify: `Cargo.toml`, `Cargo.lock` (`redis` feature `cluster-async`), `src/config.rs`, `src/shared/mod.rs`, `src/mcp/testing.rs` if its helpers need the mode, `tests/support/redis.rs`, `docs/operations.md` (one paragraph; Task 12 writes the full section), the spec if anything differs.

- [ ] **Step 1: Config** (`src/config.rs`)

```rust
/// Which Redis client `[redis]` uses (spec Decision 20).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RedisMode {
    /// One Redis or Valkey primary (`ConnectionManager`).
    #[default]
    Standalone,
    /// A cluster, including ElastiCache Serverless (`ClusterClient`).
    Cluster,
}
```

- `RedisConfig` gains `#[serde(default)] pub mode: RedisMode`.
- `RedisConfig::validate` also refuses a `key_prefix` holding `{` or `}`: the prefix goes inside the hash tag.
- Tests: `mode` defaults to standalone, `"cluster"` parses, `"sentinel"` is refused (`deny_unknown_fields` stays), and a prefix with a brace is refused with a message naming `key_prefix`.

- [ ] **Step 2: Names** (`src/shared/mod.rs`)

`Redis::key(space, id)` returns `{<prefix>}<space>:<id>` and `Redis::channel(name)` returns `{<prefix>}<name>`. Store the tagged form once at connect (`prefix: Arc<str>` holds `{okf:}`), so no call site formats braces. Update Task 1's tests of `key`/`channel`, and add one that two keys and a channel from one handle share a slot (`redis::cluster_routing::get_slot`, if public; otherwise compare the hash-tag substring).

- [ ] **Step 3: `RedisConnection`**

```rust
/// The instance's connection: standalone or cluster (spec Decision 20). Cheap
/// to clone; both arms multiplex one or more sockets and reconnect by
/// themselves.
#[derive(Clone)]
pub enum RedisConnection {
    Standalone(redis::aio::ConnectionManager),
    Cluster(redis::cluster_async::ClusterConnection),
}

impl redis::aio::ConnectionLike for RedisConnection {
    // req_packed_command, req_packed_commands and get_db delegate to the arm.
}
```

- `Redis::connection()` returns `RedisConnection`. Task 1's call sites (`connect`'s `PING`, the tests) compile unchanged apart from the type.
- `Redis::client()` is removed. Its only caller was the pub/sub plan for Task 3, which Step 5 replaces. Keep whatever the handle needs to open a subscription connection later: the `redis::Client` (standalone) or the `ClusterClient` (cluster), in a private enum.

> **Corrected (issue #570).** `Redis` keeps the parsed `redis::ConnectionInfo` (in an `Arc`) and its `RedisMode`, not a `Client` or `ClusterClient`: `subscribe` builds its own RESP3 client from the info in either mode. `redis::cluster::ClusterClientBuilder` is the builder's path in redis 1.8.2.

- [ ] **Step 4: `connect` by mode**

- Standalone: as Task 1.
- Cluster: `ClusterClientBuilder::new(vec![url])` with `connection_timeout(CONNECT_TIMEOUT)` and `response_timeout(RESPONSE_TIMEOUT)`, then `get_async_connection()`, then the same `PING` check. Install the rustls provider first, as Task 1 does.
- Errors keep Task 1's rule: the message names the variable and the error's `kind()`, never the URL. Add a test that a cluster connect to an unreachable `redis://:s3cretpw@127.0.0.1:1` fails without `s3cretpw` or the host in the message, as Task 1's standalone test does.

- [ ] **Step 5: `Redis::subscribe`**

```rust
/// One event from a subscription (spec Decision 20).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptionEvent {
    /// The server confirmed the subscription: the first time, and again after
    /// every automatic resubscribe. Messages sent before it may be lost.
    Subscribed,
    /// A message on the channel, its payload's bytes.
    Message(Vec<u8>),
    /// The library lost the connection; a `Subscribed` follows when it is back.
    Disconnected,
}

/// A live subscription; dropping it closes its connection.
pub struct Subscription { /* the dedicated connection, and the push receiver */ }

impl Subscription {
    /// The next event, or `None` once the connection is gone for good.
    pub async fn next(&mut self) -> Option<SubscriptionEvent>
}

impl Redis {
    /// Opens a dedicated RESP3 connection with a push sender and automatic
    /// resubscription, and subscribes it to `channel` (a name from
    /// [`Redis::channel`]).
    pub async fn subscribe(&self, channel: &str) -> RedisResult<Subscription>
}
```

- Standalone: a new `ConnectionManager` over the stored client with RESP3 (`ProtocolVersion::RESP3`, set on the connection info or with the URL's `protocol=resp3`), `set_push_sender(tx)` and `set_automatic_resubscription()`, then `subscribe(channel)`.
- Cluster: a new `ClusterClient` built like Step 4's with `use_protocol(ProtocolVersion::RESP3)` and `push_sender(tx)`, then `get_async_connection()` and `subscribe(channel)`.
- The push receiver maps `PushKind::Subscribe` to `Subscribed`, `Message` to `Message(payload)` (the payload is the last element of `data`), and `Disconnection` to `Disconnected`. Other kinds are skipped.
- Use an unbounded `tokio::sync::mpsc` channel only if the crate's `AsyncPushSender` needs it; a bounded one that drops on overflow would lose a `Subscribed`.
- Each subscription is its own connection, so two subscribers in one process (two `App`s in a test) never share pushes.

- [ ] **Step 6: Test support** (`tests/support/redis.rs`)

- `TestRedis::start_cluster()` starts `valkey/valkey:8.1-alpine` as a one-node cluster, as Verified facts "Cluster mode" ran it: start with `--cluster-enabled yes`, read the mapped port, `CONFIG SET cluster-announce-ip 127.0.0.1` and `cluster-announce-port <port>`, `CLUSTER ADDSLOTSRANGE 0 16383`, then wait (bounded, `tokio::time::timeout`) for `cluster_state:ok`.
- `TEST_REDIS_CLUSTER_URL` reuses a running cluster, as `TEST_REDIS_URL` does a standalone server.
- `TestRedis` records its mode, and `redis_config(&TestRedis)` sets `mode` from it, so a test picks the mode only by how it starts the server.
- The per-test random prefix stays.
- The lib-test side (`shared::testing`) gets the same `start_cluster` if the lib tests need one.

- [ ] **Step 7: Tests** (each wait bounded)

- Cluster connect and `PING` work, and `App::load_with_redis` builds with `mode = "cluster"`.
- On the cluster container, a two-key script over two `Redis::key` names runs, and the same script over two untagged names fails with a `CrossSlot` error (this proves the client is cluster-aware).

> **Corrected (issue #570).** The cluster client refuses a cross-slot script itself, before sending it, so the error reads `CrossSlot: Keys in request don't hash to the same slot` and not the server's `CROSSSLOT`. The test accepts either spelling.
- In both modes: `subscribe` yields `Subscribed`, then a `PUBLISH` on the main connection yields `Message` with the payload.
- In both modes: `CLIENT KILL TYPE pubsub` from the main connection makes the subscription yield `Disconnected` (or skip straight to it) and then `Subscribed` again, and a message published afterwards arrives. That is the resubscribe the announcement subscriber relies on (Task 3).
- The existing Task 1 tests pass in standalone mode unchanged, apart from the names' braces.

- [ ] **Step 8: Docs**

One paragraph in `docs/operations.md` under the `[redis]` config: `mode`, `"cluster"` for ElastiCache Serverless and other clusters, `rediss://` (Serverless requires TLS), and that every key shares one hash tag. Task 12 writes the full section.

- [ ] **Step 9: Gates, changelog, commit**

Changelog `changelog/<ts>-m16-task-01b-cluster-mode.md`. `feat: [redis] cluster mode for ElastiCache Serverless (M16 Task 1b)`.

---

### Task 2: `shared::ephemeral`: short-lived keyed state in memory or Redis

> **Corrected (2026-11-17, cluster mode).** Task 1b comes first. Use `Redis::connection()` (a `RedisConnection`) for every command and script, name every key a script touches in `KEYS`, and run the Redis-variant tests in both modes (`TestRedis::start` and `TestRedis::start_cluster`).

**Files:** Create `src/shared/ephemeral.rs`. Modify `src/shared/mod.rs` (`pub mod ephemeral;`), `src/oauth/expiring.rs` (module docs: it is now also `Ephemeral`'s memory variant), `tools/agentic/lessons.md`.

- [ ] **Step 1: The types**

```rust
//! Short-lived keyed state (spec Decisions 3-8): pending authorizations,
//! codes, flows and ceremonies. `Memory` is today's `ExpiringMap`, unchanged;
//! `Redis` keeps the entries in Redis under hashed keys, with Redis TTLs.

/// A key space: its key name and the version of its stored shape. Bump a
/// version whenever its value type changes shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyspace { OauthPending, OauthCode, OidcFlow, PasskeyCeremony }

impl Keyspace {
    /// The key-name part (`oauth:pending`, `oauth:code`, `oidc:flow`, `passkey:ceremony`).
    pub fn name(self) -> &'static str {
        match self {
            Self::OauthPending => "oauth:pending",
            Self::OauthCode => "oauth:code",
            Self::OidcFlow => "oidc:flow",
            Self::PasskeyCeremony => "passkey:ceremony",
        }
    }
    /// The stored shape's version (all 1 at first).
    pub fn version(self) -> u32 {
        // No `_` arm: a new key space fails to compile until it has a version.
        match self {
            Self::OauthPending | Self::OauthCode | Self::OidcFlow | Self::PasskeyCeremony => 1,
        }
    }
}

/// A per-actor index space (spec Decision 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexSpace { OidcLink, PasskeySession }  // "oidc:link", "passkey:session"

/// Values with a key and a lifetime.
pub enum Ephemeral<V> {
    /// One process's memory (no `[redis]`): bounded by `capacity`.
    Memory(ExpiringMap<V>),
    /// Redis: unbounded here (Redis's `maxmemory` bounds it).
    Redis { redis: Redis, space: Keyspace, ttl: Duration, _value: PhantomData<fn() -> V> },
}

/// The stored envelope (spec Decision 6).
#[derive(Serialize, Deserialize)]
struct Envelope<T> { v: u32, d: T }

/// An entry an [`Index`] no longer points to, to be removed from its
/// [`Ephemeral`] with [`Ephemeral::remove_stale`]. Opaque: in Redis it holds
/// the entry's hash, in memory its raw id.
#[derive(Debug, PartialEq, Eq)]
pub struct Stale(String);

/// What [`Index::replace`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Swap {
    /// The actor now points at the new entry; `Some` is the one it replaced.
    Swapped(Option<Stale>),
    /// Memory only: the index is full, nothing was stored.
    Full,
}

/// Per-actor pointers to their one live entry, with a sliding lifetime.
pub enum Index {
    Memory(ExpiringMap<String>),
    Redis { redis: Redis, space: IndexSpace, ttl: Duration },
}
```

`Stale` holds a `String` and has a `Debug` derive. In memory it is a cookie id, which a `{:?}` would print. So give it a hand-written `Debug` that prints `Stale(<redacted>)`.

- [ ] **Step 2: Write the failing tests**

Run each test body against both variants. A helper `async fn stores(ttl: Duration) -> (Vec<Ephemeral<Sample>>, TestRedis)` returns a `Memory` and a `Redis` store; the caller holds the `TestRedis` for the test. `Sample` is `#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)] struct Sample { n: u32, used: bool }`.

```rust
#[tokio::test(flavor = "multi_thread")]
async fn insert_then_take_is_single_use() {
    let (stores, _redis) = stores(TTL).await;
    for store in stores {
        let now = Instant::now();
        assert!(store.insert("a", Sample { n: 1, used: false }, now).await.unwrap());
        assert_eq!(store.take("a", now).await.unwrap(), Some(Sample { n: 1, used: false }));
        assert_eq!(store.take("a", now).await.unwrap(), None);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_takes_racing_get_one_value() {
    // Redis: two connections (two "replicas") race one GETDEL.
    let server = TestRedis::start().await;
    let (one, two) = (testing::connect(&server).await, testing::connect(&server).await);
    let a = Ephemeral::<Sample>::new(Some(&one), Keyspace::OauthCode, TTL, 10);
    let b = Ephemeral::<Sample>::new(Some(&two), Keyspace::OauthCode, TTL, 10);
    a.insert("k", Sample { n: 1, used: false }, Instant::now()).await.unwrap();
    let (x, y) = tokio::join!(a.take("k", Instant::now()), b.take("k", Instant::now()));
    assert_eq!([x.unwrap().is_some(), y.unwrap().is_some()].iter().filter(|got| **got).count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_updates_all_apply_once() {
    // 16 tasks, each `update(|s| s.n += 1)` on two Redis handles: n ends at 16.
}

#[tokio::test(flavor = "multi_thread")]
async fn update_keeps_the_fixed_lifetime_and_an_expired_entry_is_gone() {
    // Redis: ttl 300 ms; insert; sleep 150 ms; update; sleep 250 ms; take -> None.
    // Memory: the same with explicit instants (now, now + 150ms, now + 400ms).
}

#[tokio::test(flavor = "multi_thread")]
async fn a_value_of_another_version_or_shape_is_absent_and_counted() {
    // Write `{"v":99,"d":{..}}` and `not json` straight to the key with SET;
    // take -> None both times; okf_redis_errors_total{operation="decode"} == 2.
}

#[tokio::test(flavor = "multi_thread")]
async fn redis_keys_never_hold_the_presented_id() {
    // insert("cookie-id-xyz", ..); KEYS <prefix>* contains no "cookie-id-xyz",
    // and contains <prefix>oauth:code:<hashed("cookie-id-xyz")>.
}

#[tokio::test(flavor = "multi_thread")]
async fn an_index_swaps_atomically_and_returns_the_entry_it_replaced() {
    // replace("actor", "e1") -> Swapped(None); replace("actor", "e2") ->
    // Swapped(Some(stale)); store.remove_stale(&stale) removes e1; replace
    // ("actor", "e2") again -> Swapped(None) (same entry is not stale).
    // Redis: the index value is hashed("e2"), never "e2".
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redis_outage_is_busy_and_counted() {
    // Stop the container; insert -> Err(OkfError::Busy{..}); counter "set" == 1.
}
```

Run `gate.sh narrow --lib -- shared::ephemeral`. It fails to compile.

- [ ] **Step 3: Implement**

```rust
/// Writes `ARGV[2]` over `KEYS[1]` only if it still holds `ARGV[1]`, keeping
/// its remaining TTL. 1 written, 0 changed meanwhile, -1 gone (ran: see the
/// plan's verified facts).
const COMPARE_AND_SET: &str = "local cur = redis.call('GET', KEYS[1])\n\
    if cur == false then return -1 end\n\
    if cur ~= ARGV[1] then return 0 end\n\
    local ttl = redis.call('PTTL', KEYS[1])\n\
    if ttl <= 0 then return -1 end\n\
    redis.call('SET', KEYS[1], ARGV[2], 'PX', ttl)\n\
    return 1";

/// How many times `update` retries a compare-and-set that lost a race.
const UPDATE_ATTEMPTS: usize = 8;
```

- **`Ephemeral::new(redis: Option<&Redis>, space, ttl, capacity)`.** `None` gives `Memory(ExpiringMap::new(ttl, capacity))`.
- **`insert(key, value, now) -> Result<bool, OkfError>`.**
  - Memory: `Ok(map.insert(key.to_string(), value, now))`.
  - Redis: `SET <key> <envelope> PX ttl`, returning `Ok(true)`. An error is `redis.failed(RedisOp::Set, &e)`.
- **`take(key, now) -> Result<Option<V>, OkfError>`.**
  - Memory: `map.take`.
  - Redis: `GETDEL`, then `decode`. A decode failure is `undecodable(space.name())`, then `None`.
- **`update<R>(key, now, mut change: impl FnMut(&mut V) -> R) -> Result<Option<R>, OkfError>`.**
  - Memory: `map.update(key, now, change)`.
  - Redis, looping up to `UPDATE_ATTEMPTS` times:
    1. `GET` the raw bytes, returning `None` if absent.
    2. Decode them (undecodable gives `None`), and apply `change` to the decoded copy.
    3. Encode the result, and run `COMPARE_AND_SET` with `(old bytes, new bytes)`.
    4. `1` returns `Some(r)`, `-1` returns `None`, and `0` retries.
  - When the attempts run out: `OkfError::Busy { reason: "the entry is changing too fast; retry" }`.
  - The doc says that **`change` may run more than once, each time on a fresh copy, and must not move captured state out**.
- **`remove(key) -> Result<(), OkfError>`.** `DEL`.
- **`remove_stale(&Stale) -> Result<(), OkfError>`.** Memory: `map.remove(&stale.0)`. Redis: `DEL <prefix><space>:<stale.0>`, since the stale id is already hashed.
- **`#[cfg(test)] pub(crate) fn len(&self) -> usize`.** Memory only. The Redis arm panics with "len is memory-only".
- **`Index::new(redis, space, ttl, capacity)`.**
- **`Index::replace(actor_key, entry, now) -> Result<Swap, OkfError>`.**
  - Memory: today's `update_or_insert_sliding(actor_key, now, || entry.to_string(), |held| mem::replace(held, entry.to_string()))`. `None` is `Swap::Full`. A previous different from `entry` is `Swapped(Some(Stale(previous)))`; otherwise `Swapped(None)`.
  - Redis: `SET <prefix><space>:<actor_key> hashed(entry) PX ttl GET`. A previous different from `hashed(entry)` is `Swapped(Some(Stale(previous)))`.
  - `actor_key` is already a hash (callers pass today's `URL_SAFE_NO_PAD` SHA-256 of the session id).
- **Encoding.** `serde_json::to_vec(&Envelope { v: space.version(), d: value })`. Decoding refuses `v != space.version()`.
- **Bounds and spans.** `V: Serialize + DeserializeOwned + Send + Sync + 'static`. Every Redis call gets a span: `#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "redis", db.operation.name = "<op>"))]`.

- [ ] **Step 4: `lessons.md`**

Under "Locks and races", add this rule: "**A closure passed to `Ephemeral::update` may run more than once** (a Redis compare-and-set that lost a race reruns it on a fresh copy). It must compute its result from the value alone: clone captured inputs inside it, never `Option::take` or move them out, and do no I/O. Test it by racing two updates of one entry through two Redis handles (milestone 16 plan, Task 2)."

- [ ] **Step 5: Gates, changelog, commit**

`feat: Ephemeral short-lived state in memory or Redis (M16 Task 2)`.

---

### Task 3: `admin_generation` announcements over pub/sub, and the two-instance test harness

**Files:**
- Create: `src/shared/announce.rs`, `tests/multi_replica.rs`.
- Modify: `src/shared/mod.rs`, `src/app.rs`, `src/http/mod.rs`, `tests/support/server.rs`.

- [ ] **Step 1: The harness** (`tests/support/server.rs`)

```rust
impl TestServer {
    /// As [`TestServer::start_with`], with `[redis]` on `redis`, the first of
    /// a group of replicas. `poll_interval` is forced to one hour.
    pub async fn start_shared(
        catalogs_and_files: Vec<(&str, Files)>,
        redis: &TestRedis,
        make_config: impl FnOnce(&str) -> Config,
    ) -> Self

    /// Another replica: a new `App` (its own pool on the same database, its
    /// own Redis connection) serving on its own port.
    pub async fn sibling(&self, redis: &TestRedis, make_config: impl FnOnce(&str) -> Config) -> Sibling
}

/// A replica started by [`TestServer::sibling`]; it does not own the database.
pub struct Sibling { pub base: String, pub app: Arc<App>, shutdown: CancellationToken, served: Served }
```

- `serve_app` gains an `Option<okf_mcp::shared::Redis>` argument, and its `App::load` becomes `App::load_with_redis`.
- `Sibling` implements `Drop` (cancel) and `mcp_url()`.
- The caller keeps the `TestRedis` alive for the test.
- Both servers' configs get `redis: Some(redis_config(redis))`, so `App::redis()` is set.

- [ ] **Step 2: Write the failing tests** (`tests/multi_replica.rs`)

```rust
//! Two replicas over one Postgres and one Redis (milestone 16). Every test
//! sets `poll_interval = 1h`: what passes was carried by Redis.

mod support;

const WAIT: Duration = Duration::from_secs(10);

/// Retries `check` every 50 ms until it is true, or panics naming `what`
/// after [`WAIT`].
async fn eventually(what: &str, check: impl Fn() -> bool) {
    let waited = tokio::time::timeout(WAIT, async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "timed out waiting until {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_revoked_on_one_replica_is_refused_on_the_other_at_once() {
    let redis = TestRedis::start().await;
    let a = TestServer::start_shared(pg(), &redis, |_| Config::default()).await;
    let b = a.sibling(&redis, |_| Config::default()).await;
    // `a.key` writes through the store, as the CLI would, so nothing has
    // announced yet: A's own poll sees the bump and announces it.
    let admin = a.key("admin", Access::Admin, &[], None).await;
    let key = a.key("reader", Access::Read, &[], None).await;
    a.app.poll().await.expect("A's poll");
    eventually("B knows the new key", || b.app.keys().find(&key).is_some()).await;

    // POST /api/v1/keys/{id}/revoke on A settles, so A announces the bump.
    let id = b.app.keys().find(&key).expect("cached").key_id.clone();
    let revoked = reqwest::Client::new()
        .post(format!("{}/api/v1/keys/{id}/revoke", a.base))
        .bearer_auth(&admin)
        .send()
        .await
        .expect("revoke");
    assert!(revoked.status().is_success(), "{}", revoked.status());

    eventually("B refuses the revoked key", || b.app.keys().find(&key).is_none()).await;
}
```

More tests in the same file, each ending with `eventually(..)` on B:
- `a_user_deleted_on_one_replica_loses_access_on_the_other`: `DELETE /api/v1/users/{name}` on A; B's cache no longer finds the user's key.
- `a_team_suspended_on_one_replica_closes_its_catalogs_on_the_other`: `PUT /api/v1/teams/{name}/suspension` on A. A member's `get_concept` on B is refused as suspended. Use an rmcp client on B's URL; the session is opened on B, so this test needs no Task 5.
- `a_grant_revoked_on_one_replica_is_refused_on_the_other`: create a self-issued grant, revoke it through the `me` API on A; `b.app.keys().find_grant(..)` becomes `None`.
- `a_change_still_announces_after_a_non_announcing_reload_saw_it_first` (the forced interleaving from the review of PR #552):
  1. Revoke a key through the store on A, as the CLI would, so nothing settles yet.
  2. Call `a.app.catch_up_keys().await`. A's reload doesn't announce, but A now holds the new generation.
  3. Call `a.app.poll().await`, as `settle` does.
  4. B refuses the key within `WAIT`.
  5. Revert the separate `announced_generation` and the test fails: B keeps the key for an hour.
- `a_subscriber_that_lost_redis_catches_up_when_it_reconnects`:
  1. `redis.stop()`, then revoke on A (A's announcement is lost and counted), then `redis.start_again()`.
  2. B's subscriber resubscribes, reloads, and refuses the key within `WAIT`.
  3. The test skips itself on a shared `TEST_REDIS_URL`.

Each test closes with `eventually(..)`. Run `gate.sh narrow --test multi_replica`. Expect failures (no announcements: B waits an hour).

- [ ] **Step 3: `App`**

```rust
/// Reloads the key cache now if `admin_generation` moved (the announcement
/// path), serialized with poll passes through `polling`.
///
/// # Arguments
/// * `announce` - publish the new generation if it moved: `false` when this
///   reload answers an announcement (the announcer already told everyone).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn reload_keys_now(&self, announce: bool) {
    let _pass = self.polling.lock().await;
    self.reload_keys_if_changed(announce).await;
}

/// Before refusing a credential whose row is live but whose grant is not in
/// the key cache (spec Decision 14): reloads if `admin_generation` moved.
/// A no-op without Redis (one replica polls after its own changes).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn catch_up_keys(&self) {
    if self.redis.is_some() {
        self.reload_keys_now(false).await;
    }
}

/// The generation the key cache was loaded at.
pub fn loaded_admin_generation(&self) -> i64 { self.admin_generation.load(Ordering::Relaxed) }
```

`reload_keys_if_changed(&self, announce: bool)`: the poll pass calls it with `true`.

**Announcing is tracked apart from loading** (review of PR #552). Publishing only when an announcing reload moves the loaded generation loses an announcement. Here is how:
1. A commits a revocation.
2. Before A's `settle`, a non-announcing reload on A (`catch_up_keys`, or the subscriber's resubscribe) loads the new generation without publishing.
3. The `settle` poll then finds nothing moved.

So `App` gains `announced_generation: AtomicI64`, initialised to the generation loaded at start-up and written only under `polling`. At the end of every `reload_keys_if_changed(true)`, whether or not this call reloaded:

```rust
if announce {
    if let Some(redis) = &self.redis {
        let loaded = self.admin_generation.load(Ordering::Relaxed);
        // Under `polling`, so no other reload moves either counter meanwhile.
        if loaded != self.announced_generation.load(Ordering::Relaxed) {
            crate::shared::announce::publish(redis, Some(loaded)).await;
            // Stored even if the publish failed: the failure is counted, and
            // the poll loop covers a lost announcement (spec Decision 12).
            self.announced_generation.store(loaded, Ordering::Relaxed);
        }
    }
}
```

So an instance that loaded a generation from an announcement re-announces it once, at its next announcing reload. That costs one message per instance per bump, and every receiver already at that generation ignores it. The spec accepts that cost for a rule that never loses a local change's announcement.

- [ ] **Step 4: `src/shared/announce.rs`**

```rust
//! `admin_generation` announcements (spec Decision 12): the instance whose
//! reload saw a bump publishes it; every instance's subscriber reloads.

/// The channel's name under the prefix.
pub const CHANNEL: &str = "admin-generation";
/// Reconnect backoff bounds.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Publishes a generation (or, from the CLI, an empty "check now"). A failure
/// is counted and logged; the poll loop covers a lost announcement.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "redis", db.operation.name = "publish"))]
pub async fn publish(redis: &Redis, generation: Option<i64>) {
    let payload = generation.map(|g| g.to_string()).unwrap_or_default();
    let published: redis::RedisResult<usize> =
        redis.connection().publish(redis.channel(CHANNEL), payload).await;
    if let Err(error) = published {
        let _ = redis.failed(RedisOp::Publish, &error);
    }
}

/// Runs until `shutdown`: subscribes, reloads once (messages sent while it was
/// not subscribed are lost), then reloads on every announcement of another
/// generation. Reconnects with backoff when the connection ends.
#[cfg_attr(dylint_lib = "hyp_tracing", allow(uninstrumented_fn, reason = "long-lived loop: a span would stay open until shutdown"))]
pub async fn listen(app: Arc<App>, shutdown: CancellationToken) {
    let Some(redis) = app.redis().cloned() else { return };
    let mut backoff = BACKOFF_MIN;
    while !shutdown.is_cancelled() {
        match subscribe(&redis).await {
            Ok(mut messages) => {
                backoff = BACKOFF_MIN;
                app.reload_keys_now(false).await;
                loop {
                    tokio::select! {
                        () = shutdown.cancelled() => return,
                        message = messages.next() => match message {
                            Some(message) => {
                                let announced = message.get_payload::<String>().ok()
                                    .and_then(|text| text.parse::<i64>().ok());
                                if announced != Some(app.loaded_admin_generation()) {
                                    app.reload_keys_now(false).await;
                                }
                            }
                            None => { redis.count(RedisOp::Subscribe); break; }
                        },
                    }
                }
            }
            Err(error) => { let _ = redis.failed(RedisOp::Subscribe, &error); }
        }
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}
```

- `subscribe` gets `client().get_async_pubsub()`, subscribes to `redis.channel(CHANNEL)`, and returns `into_on_message()`.

> **Corrected (2026-11-17, cluster mode).** `Redis::client()` is gone (Task 1b). `listen` calls `redis.subscribe(&redis.channel(CHANNEL))` and loops over `Subscription::next()`:
- on `Subscribed` (the first time and after each automatic resubscribe), `app.reload_keys_now(false)` and reset the backoff;
- on `Message(payload)`, parse it as an `i64` and reload unless it equals `app.loaded_admin_generation()`;
- on `Disconnected`, `redis.count(RedisOp::Subscribe)`;
- on `None` or a `subscribe` error, count it, back off and subscribe again, as above.

The test `a_subscriber_that_lost_redis_catches_up_when_it_reconnects` may break the subscription with `CLIENT KILL TYPE pubsub` instead of restarting Redis, which also works on a shared server. Run one announcement test in cluster mode as well.
- `http::serve` spawns `tokio::spawn(crate::shared::announce::listen(Arc::clone(&app), shutdown.clone()))` beside the poller and awaits it like the poller, logging a panic.

- [ ] **Step 5: Run the tests, gates, changelog, commit**

`feat: announce admin_generation over Redis pub/sub (M16 Task 3)`.

---

### Task 4: The CLI announces its changes

**Files:** Modify `src/cli.rs` (`dispatch`), `src/shared/announce.rs`.

- [ ] **Step 1: Write the failing test** (`src/cli.rs` tests)

Start a `TestRedis`, subscribe to `<prefix>admin-generation` with a raw `redis::Client`, then run `dispatch` for `key revoke` with `config.redis = Some(crate::shared::testing::config(&server))` and `server.env()` as the reader. The subscriber receives one message (payload empty) within 5 s.

A second test: with the variable unset, the command still succeeds and logs `announcement skipped` at `warn` (capture with the existing test subscriber helper, if there is one; otherwise assert only success).

- [ ] **Step 2: Implement**

```rust
/// After a CLI command that may have changed keys, users, teams or grants,
/// tells every serving instance to check `admin_generation` (spec Decision
/// 13). Best effort: the change is committed whatever happens here.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn announce_from_cli(config: &Config, env: &dyn Fn(&str) -> Option<String>, metrics: &Metrics) {
    let Some(table) = &config.redis else { return };
    match Redis::connect(table, env, metrics).await {
        Ok(redis) => publish(&redis, None).await,
        Err(error) => tracing::warn!(
            kind = error.kind(),
            "announcement skipped; serving instances notice the change within poll_interval"
        ),
    }
}
```

In `dispatch`, every arm except `Serve` runs it after `Ok`, once, after the match. Run it for read-only commands too: an empty announcement makes each instance read one row, and a list of mutating commands would go stale.

- [ ] **Step 3: Gates, changelog, commit**

`feat: CLI commands announce admin changes to serving replicas (M16 Task 4)`.

---

### Task 5: MCP sessions and their bindings in Redis

> **Corrected (2026-11-17, cluster mode).** The admit, bind and release scripts take both the state key and the binding key in `KEYS`. Task 1b's hash tag puts them in one slot. Run the scripts' tests and one two-instance session test in cluster mode as well.

**Files:**
- Create: `src/shared/sessions.rs`, `tests/support/mcp_raw.rs`.
- Modify: `src/shared/mod.rs`, `src/http/sessions.rs`, `src/http/mod.rs`, `src/app.rs`, `tests/server.rs`, `tests/multi_replica.rs`, `tests/support/mod.rs`.

- [ ] **Step 1: Move the raw helpers**

Move `INITIALIZE`, `INITIALIZED`, `open_session`, `notify_on`, `stream_on` and `first_event_data` from `tests/server.rs` to `tests/support/mcp_raw.rs`. Make them `pub` and generic over a base URL (`mcp_url: &str`), not a `TestServer`. Add:
- `post_on(mcp_url, key, session, body) -> reqwest::Response`;
- `delete_on(mcp_url, key, session) -> StatusCode`.

`tests/server.rs` uses them. Run `gate.sh narrow --test server` and expect a pass.

- [ ] **Step 2: Write the failing two-instance tests** (`tests/multi_replica.rs`)

```rust
#[tokio::test(flavor = "multi_thread")]
async fn a_session_opened_on_one_replica_serves_tools_on_the_other() {
    let redis = TestRedis::start().await;
    let a = TestServer::start_shared(pg(), &redis, |_| Config::default()).await;
    let b = a.sibling(&redis, |_| Config::default()).await;
    let key = a.key("reader", Access::Read, &[], Some("pg")).await;
    a.app.poll().await.unwrap(); // A announces; B reloads
    eventually("B knows the key", || b.app.keys().find(&key).is_some()).await;

    let session = open_session(&a.mcp_url(), &key).await;
    assert_eq!(notify_on(&a.mcp_url(), &key, &session).await, 202);
    let listed = post_on(&b.mcp_url(), &key, &session, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).await;
    assert_eq!(listed.status(), 200);
    let first: serde_json::Value = serde_json::from_str(first_event_data(&listed.text().await.unwrap())).unwrap();
    assert!(first["result"]["tools"].is_array());
    let called = post_on(&b.mcp_url(), &key, &session,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_concept","arguments":{"path":"a"}}}"#).await;
    assert_eq!(called.status(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn another_key_on_that_session_is_refused_on_either_replica() {
    // owner opens on A; intruder (another key) POSTs and GETs on A and on B: 404 each.
}

#[tokio::test(flavor = "multi_thread")]
async fn a_binding_without_its_state_is_refused_on_every_replica() {
    // Open on A; DEL only <prefix>mcp:state:<hashed(session)> (as an eviction
    // would). The owner's tools/list on A, which still holds a local worker,
    // -> 404, and on a fresh B -> 404. Revert the EXISTS line in ADMIT and A
    // answers 200 while B answers 404.
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_state_without_a_binding_is_never_restored() {
    // Open on A; DEL <prefix>mcp:bind:<hashed(session)> with a raw redis client;
    // the owner's tools/list on B -> 404, and on A -> 404 too (the binding is the check).
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_keys_session_is_refused_on_every_replica() {
    // Open on A, revoke the key through A's API; owner's request on A and B -> 401.
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_session_expires_in_redis() {
    // session_idle_timeout = 2s on both; open on A; sleep 3s; tools/list on B -> 404;
    // PTTL of both keys was <= 2000 right after open.
}

#[tokio::test(flavor = "multi_thread")]
async fn use_on_one_replica_keeps_the_session_alive_for_the_other() {
    // idle 3s; open on A; every 1s for 5s, tools/list on B (200 each); then tools/list on A -> 200
    // (A's own worker idled out, so A restores from Redis).
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_ends_the_session_everywhere() {
    // Open on A. B has never received a POST or GET for this session, so it
    // holds no local copy (review of PR #552). DELETE on B -> 202; both
    // Redis keys are gone; tools/list on A -> 404, and on B -> 404.
    // Then: another key's DELETE on B -> 404, and both keys are still there
    // (the binding check still guards DELETE).
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_whose_state_write_failed_is_never_bound() {
    // Lib-level, through `http::router` on an app with Redis: the
    // `RedisSessions` test hook `fail_next_store` makes the next `store`
    // swallow a simulated failure without writing. The initialize answers
    // 503; no `mcp:bind:*` key exists; okf_redis_errors_total{operation="bind"}
    // is 1. Revert the EXISTS check in BIND and the initialize answers 200
    // with a binding that no other replica can restore.
}

#[tokio::test(flavor = "multi_thread")]
async fn redis_down_answers_503_not_a_fallback() {
    // Open on A; stop Redis; tools/list on A -> 503 (not 200 from the local session).
}
```

The last one is deliberate: with Redis on, even the instance holding the session refuses it while Redis is down. The binding check is authoritative.

- [ ] **Step 3: `src/shared/sessions.rs`**

```rust
//! MCP sessions in Redis (spec Decisions 9-11): rmcp's `SessionStore`, and
//! the binding of each session to the key that opened it.

use std::time::Duration;

use redis::{AsyncCommands, SetExpiry, SetOptions};
use rmcp::transport::streamable_http_server::session::store::{
    SessionState, SessionStore, SessionStoreError,
};

use crate::error::OkfError;
use crate::shared::ephemeral::Envelope;
use crate::shared::{hashed, Redis};
use crate::telemetry::metrics::RedisOp;

/// The version of the stored `SessionState` envelope (spec Decision 6).
const SESSION_STATE_VERSION: u32 = 1;

/// The two keys of a session, by the hash of its id.
fn keys(redis: &Redis, session: &str) -> (String, String) {
    let id = hashed(session);
    (redis.key("mcp:bind", &id), redis.key("mcp:state", &id))
}

/// `KEYS[1]` binding, `KEYS[2]` state; `ARGV[1]` key id, `ARGV[2]` idle ms.
/// Admits only the binding's key, and only while the state exists (an
/// evicted state must fail closed on every replica, not only on one that has
/// to restore it: review of PR #552). Only then does it slide both TTLs.
const ADMIT: &str = "local v = redis.call('GET', KEYS[1])\n\
    if v == false or v ~= ARGV[1] then return 0 end\n\
    if redis.call('EXISTS', KEYS[2]) == 0 then return 0 end\n\
    redis.call('PEXPIRE', KEYS[1], ARGV[2])\n\
    redis.call('PEXPIRE', KEYS[2], ARGV[2])\n\
    return 1";

/// `KEYS[1]` binding, `KEYS[2]` state; `ARGV[1]` key id, `ARGV[2]` idle ms.
/// Binds only a session whose state exists, and gives both keys the same TTL.
const BIND: &str = "if redis.call('EXISTS', KEYS[2]) == 0 then return 0 end\n\
    redis.call('SET', KEYS[1], ARGV[1], 'PX', ARGV[2])\n\
    redis.call('PEXPIRE', KEYS[2], ARGV[2])\n\
    return 1";

/// Sessions and bindings in Redis.
#[derive(Debug, Clone)]
pub struct RedisSessions { redis: Redis, idle: Duration }

impl RedisSessions {
    pub fn new(redis: Redis, idle: Duration) -> Self { Self { redis, idle } }

    /// May `key_id` use `session`? Slides both TTLs when it may.
    /// # Errors
    /// [`OkfError::Busy`] when Redis fails (the request answers 503).
    #[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "redis", db.operation.name = "admit"))]
    pub async fn admits(&self, session: &str, key_id: &str) -> Result<bool, OkfError> {
        let (bind, state) = keys(&self.redis, session);
        let admitted: i64 = redis::Script::new(ADMIT)
            .key(bind)
            .key(state)
            .arg(key_id)
            .arg(millis(self.idle))
            .invoke_async(&mut self.redis.connection())
            .await
            .map_err(|error| self.redis.failed(RedisOp::Admit, &error))?;
        Ok(admitted == 1)
    }

    /// Binds a session rmcp just issued, but only if rmcp's `store` really
    /// persisted its state: `store` swallows its errors (it must, see `load`),
    /// so a state write that failed would otherwise leave a binding that
    /// restores nowhere (review of PR #552). Aligns both TTLs.
    ///
    /// # Errors
    /// [`OkfError::Busy`] when Redis fails, or when the state is missing (the
    /// request answers 503 and the client initializes again).
    #[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "redis", db.operation.name = "bind"))]
    pub async fn bind(&self, session: &str, key_id: &str) -> Result<(), OkfError> {
        let (bind, state) = keys(&self.redis, session);
        let bound: i64 = redis::Script::new(BIND)
            .key(bind)
            .key(state)
            .arg(key_id)
            .arg(millis(self.idle))
            .invoke_async(&mut self.redis.connection())
            .await
            .map_err(|error| self.redis.failed(RedisOp::Bind, &error))?;
        if bound == 1 {
            return Ok(());
        }
        self.redis.count(RedisOp::Bind);
        tracing::warn!("an MCP session's state was not persisted; refusing to bind it");
        Err(OkfError::Busy { reason: "the shared session store is unavailable".to_string() })
    }

    /// Forgets a session after its `DELETE`: `DEL bind state`.
    #[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "redis", db.operation.name = "remove"))]
    pub async fn release(&self, session: &str) -> Result<(), OkfError> {
        let (bind, state) = keys(&self.redis, session);
        let _: i64 = self
            .redis
            .connection()
            .del(&[bind, state])
            .await
            .map_err(|error| self.redis.failed(RedisOp::Remove, &error))?;
        Ok(())
    }
}

/// A duration in whole milliseconds for `PX`, at least 1 (`PX 0` is an error).
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX).max(1)
}

// `#[async_trait]` turns each `async fn` into one returning a boxed future, which
// is what makes the trait usable as `Arc<dyn SessionStore>` (Python's ABC with
// async methods, roughly).
#[async_trait::async_trait]
impl SessionStore for RedisSessions {
    /// Never `Err`: rmcp would log the session id with it (plan Verified facts).
    /// A state without a binding, an undecodable one, or a Redis failure is
    /// `Ok(None)`: rmcp answers 404 and the client initializes again.
    async fn load(&self, session_id: &str) -> Result<Option<SessionState>, SessionStoreError> {
        let (bind, state) = keys(&self.redis, session_id);
        let fetched: redis::RedisResult<(Option<Vec<u8>>, Option<Vec<u8>>)> =
            self.redis.connection().mget(&[bind, state]).await;
        let (binding, stored) = match fetched {
            Ok(pair) => pair,
            Err(error) => {
                // Counted and logged without the id; the client re-initializes.
                let _ = self.redis.failed(RedisOp::Get, &error);
                return Ok(None);
            }
        };
        // No binding: never restore (spec Decision 10).
        let (Some(_), Some(stored)) = (binding, stored) else { return Ok(None) };
        match serde_json::from_slice::<Envelope<SessionState>>(&stored) {
            Ok(envelope) if envelope.v == SESSION_STATE_VERSION => Ok(Some(envelope.d)),
            _ => {
                self.redis.undecodable("mcp:state");
                Ok(None)
            }
        }
    }

    async fn store(&self, session_id: &str, state: &SessionState) -> Result<(), SessionStoreError> {
        let (_, key) = keys(&self.redis, session_id);
        let options = SetOptions::default().with_expiration(SetExpiry::PX(millis(self.idle)));
        match serde_json::to_vec(&Envelope { v: SESSION_STATE_VERSION, d: state }) {
            Ok(bytes) => {
                let stored: redis::RedisResult<Option<String>> =
                    self.redis.connection().set_options(key, bytes, options).await;
                if let Err(error) = stored {
                    let _ = self.redis.failed(RedisOp::Set, &error);
                }
            }
            Err(_) => self.redis.undecodable("mcp:state"),
        }
        Ok(())
    }

    async fn delete(&self, session_id: &str) -> Result<(), SessionStoreError> {
        // `bind_session` releases both keys after a successful DELETE; this
        // covers a DELETE that reached rmcp some other way.
        let (_, key) = keys(&self.redis, session_id);
        let deleted: redis::RedisResult<i64> = self.redis.connection().del(key).await;
        if let Err(error) = deleted {
            let _ = self.redis.failed(RedisOp::Remove, &error);
        }
        Ok(())
    }
}
```

Add a test hook to `RedisSessions`: `#[cfg(test)] fail_next_store: AtomicBool`. When it is set, `store` swallows a simulated failure (counted as `RedisOp::Set`) without writing, which is what `a_session_whose_state_write_failed_is_never_bound` needs. It is compiled only into tests.

The state envelope reuses Task 2's `Envelope` with its own version constant, `SESSION_STATE_VERSION = 1`. Make `Envelope` and its fields `pub(crate)` in `ephemeral.rs`. `Envelope<T>` serializes a borrowed `&SessionState` as well as an owned one: `T: Serialize` holds for `&T`.

Each `SessionStore` method gets a span. `#[tracing::instrument]` on an `#[async_trait]` method works when the attribute is placed below `#[async_trait]`'s expansion target, that is on the `async fn` inside the impl. If the lint or the macro order objects, delegate each method to an instrumented private method.

- [ ] **Step 4: `SessionBindings` and the middleware** (`src/http/sessions.rs`)

```rust
/// Where bindings live: this process (no `[redis]`; today's `SessionKeys`,
/// unchanged) or Redis.
#[derive(Debug)]
pub enum SessionBindings { Memory(SessionKeys), Redis(RedisSessions) }

impl SessionBindings {
    pub async fn admits(&self, session: &str, key_id: &str, now: Instant) -> Result<bool, OkfError>
    pub async fn bind(&self, session: &str, key_id: &str, now: Instant) -> Result<(), OkfError>
    pub async fn release(&self, session: &str) -> Result<(), OkfError>
}
```

`bind_session` uses it:
- An `Err` from `admits` answers `AppError::from(error)` (503).
- An `Err` from `bind` answers 503 instead of rmcp's response. That covers a Redis error, and a state that rmcp's `store` failed to persist (`BIND` checks it). The client then retries `initialize`, and the unbound local session idles out.
- **`DELETE` in Redis mode does not depend on a local copy** (review of PR #552). rmcp restores a session only for POST and GET (`T/tower.rs:1911, 2047`). On a replica that never held the session, `DELETE` would get rmcp's not-found answer, today's middleware would release nothing, and the replica that opened the session would go on serving it. So with Redis, an admitted `DELETE`:
  1. passes the same binding check as every request: another key gets 404, and nothing is released;
  2. then `release`s both Redis keys, before rmcp runs;
  3. then runs rmcp, which closes a local copy if there is one;
  4. answers rmcp's 2xx, or `202 Accepted` when rmcp answered 404 because it held no copy.
  A `release` error answers 503 and leaves the session in place for a retry. Every other replica's local copy is refused from then on, because its binding is gone, and it idles out.
- Memory mode keeps today's order: release after a successful rmcp `DELETE`.

`App.sessions` becomes `SessionBindings`: `Memory(SessionKeys::new(idle))` without Redis, `Redis(RedisSessions::new(redis.clone(), idle))` with it. The `SessionKeys` unit tests stay as they are.

- [ ] **Step 5: `mcp_service`**

```rust
// The builder chain is today's (allowed hosts, `with_sse_retry(None)`, the
// child cancellation token); only `mut` and the lines below are new.
let mut config = StreamableHttpServerConfig::default()
    .with_allowed_hosts(app.config().allowed_hosts.iter().cloned())
    .with_sse_retry(None)
    .with_cancellation_token(shutdown.child_token());
if let Some(redis) = app.redis() {
    config.session_store = Some(Arc::new(RedisSessions::new(redis.clone(), app.config().session_idle_timeout)));
}
```

`session_store` is a public field (`tower.rs:154`). Check that it is not hidden behind `#[non_exhaustive]` construction. If assigning to the field does not compile, use a builder; in either case fix this plan.

- [ ] **Step 6: Check the tests fail without the fix**, then gates, changelog, commit

Revert `config.session_store = ...`, and the cross-replica test fails with 404. Revert the `ADMIT` key check (always 1), and the other-key test fails. Restore with `git checkout` (`lessons.md`: touch after restoring). Commit `feat: MCP sessions and key bindings in Redis (M16 Task 5)`.

---

### Task 6: Access-token revocation across replicas, and grant catch-up

**Files:** Modify `src/oauth/access.rs`, `src/oauth/mod.rs`, `src/oauth/token.rs`, `src/app.rs`.

- [ ] **Step 1: Write the failing tests** (`src/oauth/access.rs` tests; `mcp::testing::replica_pair` with OAuth config)

1. `a_token_revoked_on_one_replica_is_refused_on_the_other`:
   1. Mint a token on A (through the test helpers' code exchange).
   2. Verify it on B, so B caches it.
   3. Revoke it through A's `/oauth/revoke`.
   4. Verify on B again. It must be `None` at once, with no wait.
2. `a_lost_counter_still_clears_every_cache`:
   1. Set the counter to 1 by hand (`SET <prefix>oauth:access-revocations 1`), the case the review of PR #552 named.
   2. Cache the token on B, so B has seen 1.
   3. `DEL` the counter, then revoke on A. The counter is recreated from a random seed, not from 0, so it is not 1 again.
   4. B refuses at once.
   5. Revert the seeding (plain `INCR`) and the test fails: B sees 1 again and serves the cached token.
3. `a_missing_counter_seen_by_a_reader_clears_its_cache`: cache on B, then `DEL` the counter, with no revocation. B's next verify seeds a new counter, finds it differs, and clears its cache, so the next verify goes to the database.
4. `a_redis_failure_skips_the_cache_and_still_answers_from_postgres`: stop Redis; verify on B queries the database and returns the principal (live token); nothing is cached (`cache.tokens.len()` unchanged).
5. `an_entry_cached_with_redis_is_rechecked_after_60_s`: use `remember` with an explicit `cached_at` 61 s ago, and verify reads the database (count the queries, or revoke the row directly in SQL without `forget` and see `None`).
6. `a_token_minted_on_one_replica_is_accepted_on_the_other_before_any_announcement`:
   1. Approve and exchange on A. The codes are still per instance until Task 7, so both happen on A.
   2. Verify the new access token on B. Lib apps run no `http::serve`, so no subscriber has told B about the grant, and B's key cache does not hold it yet.
   3. `verify` on B succeeds through `catch_up_keys`.
   4. Revert the catch-up and `verify` returns `None`.

   The exchange's own catch-up (`token.rs:411`) is tested in Task 7, once a code can cross instances.

- [ ] **Step 2: Implement**

```rust
/// The Redis counter every revocation bumps (spec Decision 15).
const REVOCATIONS: &str = "oauth:access-revocations";
/// With Redis on, how long a cached token is trusted without a database look.
const SHARED_MAX_AGE: Duration = Duration::from_secs(60);

/// Reads the counter, creating it first from `ARGV[1]` (a random seed) if it
/// is missing. A counter Redis lost therefore never restarts at a value an
/// instance has already seen (review of PR #552).
const READ_REVISIONS: &str = "if redis.call('EXISTS', KEYS[1]) == 0 then\n\
        redis.call('SET', KEYS[1], ARGV[1])\n\
    end\n\
    return redis.call('GET', KEYS[1])";

/// Bumps the counter, seeding it the same way first.
const BUMP_REVISIONS: &str = "if redis.call('EXISTS', KEYS[1]) == 0 then\n\
        redis.call('SET', KEYS[1], ARGV[1])\n\
    end\n\
    return redis.call('INCR', KEYS[1])";
```

The seed is a random non-negative `i64` below `2^62`, from `rand`, or from the bytes `keys::generate_with` already draws. Either leaves room to `INCR`. Two instances seeding at once is harmless: `SET` runs inside the script, and Redis runs scripts one at a time. A collision with a value an instance saw before has a chance of about 2^-62.

- `AccessTokens::new(redis: Option<Redis>)` and `OAuth::new(config, redis: Option<&Redis>)`. `App::load_with_redis` passes `redis.as_ref()`.
- `Cache` gains `seen_revisions: Option<i64>`. Each cached entry gains `cached_at: Instant`, through a small wrapper struct in the map.
- `verify`, when Redis is on:
  - Run `READ_REVISIONS` (a missing counter is seeded, never read as 0). The result is parsed as `i64`.
  - If the counter differs from `seen_revisions`: clear `tokens`, bump `generation`, and set `seen_revisions`.
  - On a Redis error: count it, and look the token up in the database without `remember`.
  - A hit older than `SHARED_MAX_AGE` is treated as a miss.
  - Without Redis, everything is as today.
- `forget` becomes `pub async fn forget(&self, hashes: &[[u8; 32]])`. It does the local forget as today, then, with Redis, runs `BUMP_REVISIONS` (an error is counted). Update its callers at `token.rs:273, 530` and `access.rs:300, 381, 409` with `.await`.
- Grant catch-up:
  - In `verify`: if `app.keys().find_grant(&live.grant_id)` is `None` and `app.redis().is_some()`, call `app.catch_up_keys().await` and look again.
  - Do the same at `token.rs:411` (exchange) and in the refresh path's grant lookup (`token.rs:281`).

- [ ] **Step 3: Revert-check, gates, changelog, commit**

`feat: access-token revocation and grant catch-up across replicas (M16 Task 6)`.

---

### Task 7: OAuth authorization codes on `Ephemeral`

**Files:** Modify `src/oauth/mod.rs`, `src/oauth/state.rs`, `src/oauth/token.rs`, `src/oauth/authorize.rs` (the insert at `:1010` only).

- [ ] **Step 1: Write the failing tests** (`src/oauth/token.rs` tests, `replica_pair` with OAuth)

- `a_code_minted_on_one_replica_is_exchanged_on_the_other_once`: approve on A, then exchange on B (200). B has no subscriber in a lib pair, so this also covers the exchange's grant catch-up (Task 6, `token.rs:411`): revert it, and the answer is "the grant was revoked". Then exchange again on A → `invalid_grant` "already used", and the family is revoked: the access token from the first exchange is refused by `verify` on both.
- `two_replicas_racing_one_code_leave_no_usable_token`: `tokio::join!` the exchange on A and B.
  - At most one answers 200, and every other answer is `invalid_grant`.
  - No token minted by either is usable: `verify` refuses it on both replicas.
  - Both may fail. If B sets `replayed` before A's check after commit (`token.rs:427-440`), A revokes what it minted and refuses too. The protocol allows that, and the test must not weaken it (review of PR #552).
  - The uncontended test above proves a single exchange succeeds.
- `an_expired_code_is_refused_on_either_replica`: `PEXPIRE <prefix>oauth:code:<code_key> 1`, then exchange → "unknown or expired code".
- The existing memory tests (`a_replayed_code_is_refused_and_revokes_what_it_minted`, `:670-`) pass unchanged apart from `.await`.

- [ ] **Step 2: Implement**

- `AuthorizationCode` derives `Serialize, Deserialize`.
- `OAuth::codes` becomes `Ephemeral<AuthorizationCode>`, built with `Ephemeral::new(redis, Keyspace::OauthCode, AUTHORIZATION_CODE_TTL, MAX_AUTHORIZATION_CODES)`.
- `token.rs:371` becomes `oauth.codes.update(&key, started, |stored| ...).await?`. The closure already clones and touches only `stored`, so it is idempotent: running it twice on a fresh copy gives the same answer.
- The `:433-435` read becomes `.update(..).await`. A Redis error there is treated as `replayed = true`, so it fails closed and revokes: an exchange that cannot prove the code was not replayed must not keep its tokens. A comment says so.
- `authorize.rs:1010` becomes `.insert(..).await`, where `false` or an `Err` answers the existing "busy" page.
- The memory key is `code_key(code)`, as today. The Redis key is `oauth:code:<code_key>`. `Ephemeral` hashes the key it is given, and `code_key` is already a hash, so the Redis key name is `hashed(code_key(code))`. That is harmless. Note it in a comment rather than special-casing.

- [ ] **Step 3: Revert-check, gates, changelog, commit**

`feat: OAuth authorization codes in shared state (M16 Task 7)`.

---

### Task 8: OAuth pending authorizations on `Ephemeral` (the `authorize.rs` conversion)

**Files:**
- Modify: `src/oauth/mod.rs`, `src/oauth/state.rs`, `src/oauth/authorize.rs`.
- Modify for serde: `src/store/keys.rs`, `src/store/oauth/clients.rs`, `Cargo.toml` (`chrono` `serde`).

- [ ] **Step 1: Serde**

- `Access` gets `#[derive(Serialize, Deserialize)] #[serde(rename_all = "lowercase")]` (the spellings of `as_str`). Add a unit test that its JSON matches `as_str` for all three.
- `ClientKind` gets the same treatment.
- `ClientRecord` derives `Serialize, Deserialize`, which needs `chrono`'s `serde` feature.
- `PendingAuthorization`, `AcceptedCode` and `UserConsent` derive them too, keeping their hand-written `Debug`.

- [ ] **Step 2: Write the failing tests** (`src/oauth/authorize.rs` tests, `replica_pair`)

- `consent_started_on_one_replica_is_approved_on_the_other_and_exchanged_on_the_first`: `GET /oauth/authorize` on A, the sign-in code step and Approve on B (redirect with a code), then the exchange on A → 200.
- `a_consent_page_approves_once_across_replicas`: on the user path, POST the same Approve form to A and B concurrently. One redirects with a code; the other gets the expired page (the nonce was spent by one CAS).
- `wrong_codes_count_across_replicas`: alternate wrong sign-in codes between A and B, `MAX_FAILED_ATTEMPTS` times. The pending authorization is dropped, and the next try on either replica gets "Too many wrong sign-in codes." or the expired page.
- `an_expired_pending_authorization_is_refused`: `PEXPIRE` its key, then POST → the expired page.
- The memory tests pass unchanged apart from `.await`.

- [ ] **Step 3: The conversion**

- `OAuth::pending` becomes `Ephemeral<PendingAuthorization>`. The 9 call sites listed in Verified facts each gain `.await`.
- A Redis `Err` answers `server_error(&error, &brand)`, which renders the 503 page for `Busy`. Check that it does; if `server_error` renders 500 for every error, add a `Busy` → 503 branch.
- **Make the `:762` closure idempotent.** It does `fresh.take()` on a captured `Option`; under compare-and-set a second run sees `None` and refuses a valid Approve. Change it to clone `fresh` inside the closure:

```rust
let pending = oauth.pending.update(&form.pending, now, |pending| {
    let matches: bool = pending.csrf.as_bytes().ct_eq(form.csrf.as_bytes()).into();
    matches.then(|| {
        let decided = approval(pending.accepted.as_ref(), form.summary.as_deref());
        // Cloned, not taken: under Redis this closure can run again on a fresh
        // copy (Ephemeral::update), and the second run must see the same token.
        let claimed = fresh.clone().is_some_and(|fresh| {
            claim_consent(pending.consent.as_mut(), form.summary.as_deref(), fresh)
        });
        (pending.clone(), decided, claimed)
    })
}).await;
```

- The `:923` closure moves `accepted` in. Change it to `pending.accepted = Some(accepted.clone());`.
- Check every other closure against the `lessons.md` rule (Task 2).
- Write a unit test that drives `Ephemeral::update` through a forced retry (the second test of Step 2 does this). Then revert the `.clone()` and see it fail.

- [ ] **Step 4: Gates, changelog, commit**

`feat: OAuth pending authorizations in shared state (M16 Task 8)`.

---

### Task 9: OIDC flows and link flows on `Ephemeral`

**Files:** Modify `src/oidc.rs`, `src/http/oidc.rs`, `src/app.rs` (constructor argument).

- [ ] **Step 1: Write the failing tests** (`src/http/oidc.rs` tests, `replica_pair` with the OIDC provider stub those tests already use)

- `a_sign_in_started_on_one_replica_completes_on_the_other`: `start` on A, `callback` on B with A's flow cookie → signed in.
- `a_flow_finishes_once_across_replicas`: replay the same callback on A after B → refused.
- `a_link_flow_replaces_the_sessions_previous_one_across_replicas`: start link flows for one session on A, then on B. The first flow's callback fails, and the index is one key.
- `no_raw_session_id_reaches_redis`: after a link start, scan the prefix's keys and values (`KEYS`, `GET`). None contains the raw console session id.
- `a_link_flow_finishes_only_in_the_session_that_started_it`: start a link flow in session S1, then send the callback with S1's flow cookie but another session S2's console cookie, on the other replica. It is refused, and nothing is linked. With S1's cookie it links, and S1 is the session kept. Also run this test on memory: the hash comparison replaces the raw comparison on both variants.
- The existing `flows.len()` test (`src/http/oidc.rs:1025`) stays on memory.

- [ ] **Step 2: Implement**

- `Flow`, `FlowSecrets` and `LinkRequest` derive `Serialize, Deserialize`, keeping their redacting `Debug`.
- **`LinkRequest.session_id` becomes `session_hash`** (Decision 7): `URL_SAFE_NO_PAD.encode(keys::hash(session_id))`, the same form `by_session` and `link_flows` already use as keys.
  - The callback reads the finishing request's own console session. It refuses the flow unless that session's id hashes to `session_hash`, with the same refusal as today's mismatch.
  - It then passes that request's raw session id, not a stored one, wherever the store needs it (keeping it when the others end).
  - This is the same mechanism as Task 10's.
- `flows` becomes `Ephemeral<Flow>` (`Keyspace::OidcFlow`, `FLOW_TTL`, `MAX_FLOWS`), and `link_flows` becomes `Index` (`IndexSpace::OidcLink`).
- `store_flow` becomes `async` and returns `Result<bool, OkfError>`. A `Swap::Swapped(Some(stale))` is removed with `flows.remove_stale`, and `Swap::Full` removes the new flow and returns `false`, as today.
- `OidcProviders::new(config, env, redis: Option<&Redis>)`.
- `link_starts` (a rate limit) stays per instance (#357).

- [ ] **Step 3: Gates, changelog, commit**

`feat: OIDC flows in shared state (M16 Task 9)`.

---

### Task 10: Passkey ceremonies on `Ephemeral`

**Files:** Modify `src/http/passkeys.rs`, `src/app.rs` (constructor argument), `Cargo.toml` (`webauthn-rs` feature `danger-allow-state-serialisation`).

- [ ] **Step 1: Write the failing tests** (`src/http/passkeys.rs` tests, `replica_pair`)

- `a_registration_begun_on_one_replica_finishes_on_the_other`: register by setup link (`register_by_setup`, `:1052`), with the start on A and the finish on B. The passkey is stored and signs in.
- `a_sign_in_begun_on_one_replica_finishes_on_the_other`: `alice_with_passkey`, then the start on A and the finish on B → session.
- `a_ceremony_cannot_be_finished_twice_across_replicas`: finish on B, then post the same assertion with the same cookie to A → 410 `CEREMONY_EXPIRED`.
- `a_session_registration_finishes_only_in_the_session_that_started_it`:
  1. Start a registration in console session S1 on A.
  2. Finish on B with S1's ceremony cookie but session S2's console cookie: 403, "this passkey request belongs to another session", no passkey stored, and S2's ceremony is not consumed for S1.
  3. Restart in S1 and finish with S1's cookie: 201, and with `keep_sessions: false` every session but S1 ends.
  4. Run it on memory too.
- `no_raw_session_id_reaches_redis`: after a session registration starts, no Redis key or value contains the raw console session id.
- `one_sessions_registrations_hold_one_ceremony_across_replicas`: start registrations for one console session on A, then on B. The first ceremony's finish fails, and the prefix holds one `passkey:ceremony:*` and one `passkey:session:*` key.
- The memory tests (`one_actors_repeated_starts_hold_one_ceremony`, `:1648`, and the expiry test, `:1320`) stay on memory, unchanged apart from `.await`.

- [ ] **Step 2: Implement**

- Turn on `danger-allow-state-serialisation`. Add a `Cargo.toml` comment: the state goes to Redis, server-side, never to the client.
- `Ceremony`, `Registration`, `Pending` and `RegisterVia` derive `Serialize, Deserialize`. Still no `Debug`.
- `ceremonies` becomes `Ephemeral<Ceremony>` (`Keyspace::PasskeyCeremony`, `CEREMONY_TTL`, `MAX_CEREMONIES`), and `by_session` becomes `Index` (`IndexSpace::PasskeySession`).
- `remember` and `take_ceremony` become `async` with `Result`, and their call sites (`:513, :539, :726, :834`) gain `.await`. `take_ceremony` uses `take` (`GETDEL`).
- `Passkeys::new(config, redis: Option<&Redis>)`.
- **`RegisterVia::Session { session_id }` becomes `session_hash`** (Decision 7): `URL_SAFE_NO_PAD.encode(keys::hash(session_id))`, the form `session_key` (`:366-373`) already computes for `by_session`.
  - `session_key` returns the stored hash directly.
  - At finish (`:946-958`), the check `session_id != session.id` becomes `session_hash != URL_SAFE_NO_PAD.encode(keys::hash(&session.id))`. `session` is the finishing request's own console session, from `signed_in`.
  - `SessionRegistration.session_id` is then `&session.id`, the request's own raw id, never a stored one.
  - No raw console session id is stored, in memory or in Redis.

- [ ] **Step 3: Gates, changelog, commit**

`feat: passkey ceremonies in shared state (M16 Task 10)`.

---

### Task 11: `/healthz` reports Redis

**Files:** Modify `src/http/mod.rs` (`Health`, `healthz`, its tests).

- [ ] **Step 1: Write the failing tests** (`src/http/mod.rs` tests)

```rust
#[tokio::test(flavor = "multi_thread")]
async fn healthz_without_redis_reports_null_and_keeps_its_status() {
    let test = app_with(&[("pg", &[])], Config::default()).await;
    let router = router(Arc::clone(&test.app), &CancellationToken::new());
    let response = router.oneshot(Request::get("/healthz").body(Body::empty()).expect("request")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    // The key is present and null (lessons.md: check the key, not `body["redis"]`).
    assert_eq!(body.get("redis"), Some(&serde_json::Value::Null));
}

#[tokio::test(flavor = "multi_thread")]
async fn healthz_reports_redis_without_changing_its_status() {
    let (test, redis) = app_with_redis(Config::default()).await; // Task 1's helper
    let router = router(Arc::clone(&test.app), &CancellationToken::new());
    let get = || Request::get("/healthz").body(Body::empty()).expect("request");
    let up = json_body(router.clone().oneshot(get()).await.unwrap()).await;
    assert_eq!(up["redis"], "ok");
    if redis.stop().await.is_none() {
        eprintln!("shared TEST_REDIS_URL: the outage half of this test is skipped");
        return;
    }
    let response = router.oneshot(get()).await.unwrap();
    // A Redis outage never makes the replica unready: the database decides.
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["redis"], "unavailable");
}
```

- Update the existing exact-body test (`healthz_reports_an_unreachable_database_as_503`, `:1107-1122`) to include `"redis": null`.
- Run `gate.sh narrow --lib -- http::tests::healthz`. Expect failures.

- [ ] **Step 2: Implement**

- `Health` gains `redis: Option<&'static str>`, documented as "`"ok"` or `"unavailable"`; `null` without `[redis]`. It never changes the status."
- `healthz` probes with `PING` under the same `HEALTH_TIMEOUT` as the database. Run the two probes concurrently with `tokio::join!`, so the route's deadline is unchanged.
- A failed or late `PING` is `"unavailable"`, counted as `RedisOp::Get` through `redis.failed` (rename the operation if a `Ping` variant reads better, and fix Task 1's list).
- The status code still follows the database alone, and the doc comment says so.

- [ ] **Step 3: Gates, changelog, commit**

`feat: /healthz reports Redis without changing its status (M16 Task 11)`.

---

### Task 12: Operations docs for multi-replica serve

**Files:** Modify `docs/operations.md`, `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`.

- [ ] **Step 1: `docs/operations.md`**

- **Section 1, "Why one replica"**: rewrite it as the spec's "Docs" section says.
- **Corrected (2026-11-17, cluster mode).** The Redis section also covers `mode`: `"cluster"` for ElastiCache Serverless (TLS required, so `rediss://`) and other clusters, the single hash tag and why, and that Serverless's eviction and `maxmemory` settings are managed by AWS.
  - Without `[redis]`: one replica, and why: the in-memory state table.
  - With `[redis]`: several replicas, and the caveats:
    - per-instance `GET /mcp` streams;
    - catalog read lag up to `poll_interval`;
    - per-instance rate limits (#357);
    - the mail worker: run it on one replica (`[mail] worker = false` on the others) until #458;
    - the hourly prune on every instance (idempotent);
    - `okf.mcp.sessions.active` counting restored sessions on each instance.
- **Section 3**:
  - add the `[redis]` keys to the key table and the TOML example;
  - add the URL variable to "The environment", saying it may hold a password and belongs in a Secret.
- **A new "Redis" section after "Postgres"**:
  - the supported topologies: standalone or a managed primary; no Cluster, no Sentinel;
  - the key layout table (from the spec) and TTLs;
  - what fails closed when Redis is down (the spec's Decision 16), and `okf.redis.errors`;
  - eviction (`noeviction` or `volatile-*`);
  - TLS (`rediss://`, private CAs via `SSL_CERT_FILE`) and AUTH;
  - sizing: entries are about 1-4 KB; the start limits times replicas bound the anonymous entries; MCP sessions are about 1 KB each.
- **Section 5's Deployment**:
  - `replicas: 1` gets a comment naming the real reason;
  - a second, commented variant with `[redis]`: `replicas: 2`, `strategy: RollingUpdate`, the `OKF_REDIS_URL` env from a Secret;
  - a note that the Service needs no session affinity.
- **Health (section 7)** documents the `redis` field, and that it never changes the status code (Decision 8).
- **The metric reference** gains `okf.redis.errors`, and the span reference the Redis spans.

- [ ] **Step 2: The server design**

Add a dated note at `:280-281` and at `:1335`: "(Corrected, M16, <date>.) Several replicas are supported with `[redis]`; see `2026-11-17-okf-multi-replica-design.md`."

- [ ] **Step 3: #357**

Comment on #357: the shared Redis handle (`shared::Redis`, `App::redis`) exists now, and shared buckets belong there.

- [ ] **Step 4: Gates, changelog, commit**

`docs: running several replicas with Redis (M16 Task 12)`.

---

### Task 13: Smoke test, README, milestone changelog

**Files:** Modify `README.md`; create `changelog/<ts>-m16-milestone.md`.

- [ ] **Step 1: Smoke test** (by hand, on this machine; never the kind smoke test)

1. `docker run -d --rm -p 16379:6379 --name <unique-name> redis:7.4-alpine`, with a password set by `--requirepass` from a generated value. Never print it.
   > **Corrected (2026-11-17, cluster mode).** Repeat the checks below once with `mode = "cluster"` on a one-node `valkey/valkey:8.1-alpine` cluster, set up as in Verified facts "Cluster mode".
2. Run two `okf-mcp serve` processes on one Postgres (`scripts/test-pg.sh` or a container), with `[redis] url_env = "OKF_REDIS_URL"`, `listen` on two ports, `public_url` on the first.
3. Check:
   - initialize on the first port and `tools/list` on the second;
   - revoke the key with `okf-mcp key revoke` (the CLI path), and see both refuse within 2 s;
   - `redis-cli --scan --pattern 'okf:*'` shows only hashed ids;
   - stop the Redis container, and see `tools/list` answer 503 and `okf_redis_errors_total` rise on `/metrics`;
   - start it again, and see the subscriber resubscribe (a `keycache` reload in the log).
4. Record the commands and trimmed output in the milestone changelog. Remove the containers and processes you started, by their own ids.

- [ ] **Step 2: README**

Under deployment, add a paragraph: one replica by default; several with `[redis]`, linking the operations guide.

- [ ] **Step 3: Milestone changelog**

Summarise the tasks, the decisions, the deviations (including every `Corrected` note), the follow-ups (#357, #458), and the open questions that were answered.

- [ ] **Step 4: Gates, commit**

`docs: multi-replica serve close-out (M16 Task 13)`.

---

## Self-review

- **Spec coverage.**
  - Decision 1: Task 1.
  - Decisions 2-8: Tasks 1 and 2.
  - Decisions 9-11: Task 5.
  - Decision 12: Task 3; 13: Task 4; 14: Tasks 3 and 6; 15: Task 6.
  - Decision 16: Tasks 2, 3, 5 and 6, and the outage tests.
  - Decision 17: Tasks 1 and 2, and every Redis call's span.
  - Decision 18: Task 1.
  - Decision 19 (`/healthz`): Task 11.
  - Decision 20 (cluster mode): Task 1b, with both-mode tests in Tasks 2, 3 and 5.
  - Docs: Tasks 1 and 12.
- **The issue's acceptance criteria.**
  - Unchanged without `[redis]`: every task's memory tests.
  - The two-instance MCP session: Task 5.
  - A different key gets 404, and an unbound session is refused: Task 5.
  - Revocation reaches every instance: Tasks 3 and 5.
  - OAuth across instances, and a code is single-use: Tasks 7 and 8.
  - Passkeys across instances, and once only: Task 10.
  - OIDC across instances: Task 9.
  - Expiry: Tasks 2, 5, 7 and 8.
  - Start-up refusal without the URL in the message: Task 1.
  - Testcontainers Redis: Task 1.
  - Docs: Task 12.
  - The gates: every task.
- **Names used across tasks:**
  - `shared::Redis` (`connect`, `key`, `channel`, `connection`, `client`, `failed`, `undecodable`, `count`) and `shared::hashed`;
  - `RedisOp`, `Ephemeral`, `Keyspace`, `Index`, `IndexSpace`, `Swap`, `Stale`;
  - `RedisSessions`, `SessionBindings`;
  - `announce::{publish, listen, announce_from_cli, CHANNEL}`;
  - `App::{redis, load_with_redis, reload_keys_now, catch_up_keys, loaded_admin_generation}`;
  - `TestRedis`, `replica_pair`, `TestServer::{start_shared, sibling}`, `Sibling`.

## Open questions for the maintainer

None. The three questions raised during planning were answered on 2026-11-17 and are recorded as Decisions 7, 8 and 9 above. The cache type was settled the same day (Decision 11).

Still to check against the real cache once it is reachable from a deployment: that ElastiCache Serverless accepts RESP3 (`HELLO 3`) and the client's cluster discovery, as the one-node Valkey cluster does. Task 13's hand smoke test can't reach it, so the first deploy checks it.

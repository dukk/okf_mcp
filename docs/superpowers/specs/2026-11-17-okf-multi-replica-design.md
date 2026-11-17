# okf-mcp multi-replica serve: design

Status: designed 2026-11-17 (issue #551); to be delivered in milestone 16.

## Goal

`okf-mcp serve` can run as several replicas behind one Service. The
short-lived state that today lives in one process's memory moves to an
optional Redis:

- MCP sessions and the key each session is bound to;
- OAuth pending authorizations and authorization codes;
- OIDC sign-in and link flows;
- passkey ceremonies.

Two more caches learn about changes on every instance at once:

- the key cache, through a Redis pub/sub announcement of every
  `admin_generation` bump;
- the OAuth access-token cache, through a revocation counter in Redis.

**Without `[redis]` nothing changes.** Everything stays in memory, the
code paths are today's, and one replica is still required.

## Why one replica today

`docs/operations.md` says "`publish` takes each catalog's publish lock
inside the process". That is not what makes several replicas unsafe.

**Publish is already safe across processes.** `store::publish` takes two
locks:

- an in-process mutex (`CatalogState::publishing`, `src/catalog.rs:177-182`),
  which orders publishes within one process;
- a per-catalog advisory lock, `pg_advisory_xact_lock` (`catalogs::lock`),
  inside the publish transaction, which orders publishes, imports and
  deletes of a catalog across every process.

Changeset writes lock the changeset row in Postgres, and overlays are
cached only while the changeset's `version` and the catalog's `generation`
match.

What breaks with N replicas is the state held in one process's memory:

| State | Where | What fails on another instance |
|-------|-------|--------------------------------|
| MCP sessions | rmcp `LocalSessionManager` (`src/http/mod.rs:268-296`) | a request naming a session another instance created gets 404 |
| Session-to-key bindings | `SessionKeys` (`src/http/sessions.rs`) | a restored session has no binding, so the key check (M5 Decision 2) cannot run |
| OAuth pending authorizations and codes | `OAuth::pending`, `OAuth::codes` (`src/oauth/mod.rs:79-81`) | consent or the token exchange lands elsewhere: "unknown" request or code |
| OIDC flows and link flows | `OidcProviders::flows`, `link_flows` (`src/oidc.rs:263-269`) | the callback lands elsewhere and the flow is unknown |
| Passkey ceremonies | `Passkeys::ceremonies`, `by_session` (`src/http/passkeys.rs:80-85`) | finishing a registration or sign-in elsewhere fails |
| The key cache | `Keys`, reloaded when `admin_generation` moves (`App::reload_keys_if_changed`, `src/app.rs:742`) | a key revoked on A still works on B until B's next poll |
| The access-token cache | `AccessTokens` (`src/oauth/access.rs`) | a token revoked on A still works on B until the token expires |
| Rate-limit buckets | `src/http/limits.rs`, `src/oauth/ratelimit.rs`, OIDC `link_starts` | each instance counts separately (#357) |
| The mail worker | `mail.worker` | #458 |

The last two are not part of this milestone.

## Non-goals

- **A shared catalog snapshot**, or pub/sub invalidation of snapshots. Each
  instance refreshes its snapshots on its own poll. A publish on A is
  visible on B within `poll_interval`. This is documented, not fixed.
- **SSE resumption (`EventStore`).** `sse_retry` stays off. A `GET /mcp`
  stream belongs to one instance, and a client that loses it reconnects.
- **Rate limits (#357) and the mail worker (#458).** The Redis connection
  added here is the natural home for shared rate limits; #357 gets a note.
- **Redis Cluster and Sentinel.** One standalone Redis, or a managed
  endpoint that behaves as one primary. The Lua scripts touch two keys
  without hash tags, which a cluster would refuse.
- **Running Redis.** The deployment supplies it; the docs say what it
  needs.

## Decisions

1. **Redis is optional.** A `[redis]` table with `url_env` (the name of the
   environment variable holding the URL, which may carry a password) and
   `key_prefix` (default `"okf:"`), and no other keys
   (`deny_unknown_fields`).
   - Without the table, no Redis code runs, and no Redis connection is made.
   - With it, `serve` refuses to start when the variable is unset or blank,
     the URL does not parse, or Redis does not answer `PING`.
   - No error message, log line or span ever carries the URL or its value.
     Errors name the variable and the Redis error's kind.
2. **The `redis` crate, 1.8.2, pinned exactly.** It is used with
   `default-features = false` and the features `tokio-comp`,
   `connection-manager`, `script` and `tokio-rustls-comp`. 1.8.2 is the
   newest stable release; 2.0 is still a release candidate. One
   `ConnectionManager`, which multiplexes and reconnects, serves the whole
   instance. Pub/sub uses one dedicated `aio::PubSub` connection.
   - `fred` was considered. It has nothing this design needs that `redis`
     lacks, and `redis` is the more widely used client.
   - `rediss://` needs a process-wide rustls `CryptoProvider`, because the
     lock file builds rustls with both `ring` and `aws-lc-rs` (see the plan's
     verified facts). `serve` installs `ring`'s provider before it connects.
3. **One abstraction for short-lived keyed state, chosen at start-up.**
   `shared::Ephemeral<V>` is an enum with a `Memory` variant (today's
   `ExpiringMap`) and a `Redis` variant. Both expose `insert`, `take`,
   `update` and `remove`. `shared::Index` holds the per-actor indexes.
   - It is an enum, not a trait object, so the memory path stays a direct
     call with no serialization: the "nothing changes" rule holds in the
     code as well as in behaviour.
   - Every method takes the caller's `now` (`Instant`), which only the
     memory variant reads. Redis entries expire by Redis TTL (`SET ... PX`).
4. **`take` is atomic.** `GETDEL` (Redis 6.2+), so a passkey ceremony or an
   OIDC flow can be finished once across every instance.
5. **`update` is an atomic compare-and-set.** OAuth's pending
   authorizations and codes are changed in place under a lock today. In
   Redis, `update` reads the value, applies the change locally, then writes
   it back with a Lua script. The script writes only if the stored bytes
   are still the ones read, and keeps the entry's remaining TTL. On a
   mismatch it retries, up to 8 times, then answers `Busy` (503).
   - The authorization code keeps its replay detection: a used code stays
     with `used = true` until it expires. An exchange that finds it used
     sets `replayed`, and the family is revoked as today. Plain `GETDEL`
     would lose that, so codes use `update`, not `take`.
   - The change closure may run more than once, so it must be idempotent
     on a fresh copy (no `Option::take` of a captured value). The plan
     names the closures this changes, and `tools/agentic/lessons.md` gets
     the rule.
6. **Values carry a version.** A Redis value is JSON
   `{"v": <keyspace version>, "d": <value>}`. A value of another version, or
   one that does not decode, counts as absent and is counted as an error
   (`decode`). After a rolling deploy that changes a value's shape, the
   flow in progress restarts; nothing is misread.
7. **No credential in a Redis key name, and none in an index value.**
   No raw console session id is stored at all (the maintainer,
   2026-11-17). A passkey registration or an OIDC link flow started from a
   console session stores the session id's hash. It finishes only if the
   finishing request's own session cookie hashes to it, and then uses that
   request's own id. Every
   id a client presents (MCP session id, pending authorization id, flow
   cookie, ceremony cookie) is hashed (hex SHA-256) before it becomes part
   of a key, as authorization codes already are. Per-actor indexes store
   the hash of the entry they point to. Values hold what the flow needs,
   as in memory today: PKCE verifiers, nonces, CSRF tokens, WebAuthn
   state, but never a raw console session id. So Redis is a secret store, with the same trust as Postgres:
   password-protected, private network, TLS where it crosses hosts.
8. **The global caps go; the per-actor caps stay.**
   - `MAX_PENDING`, `MAX_AUTHORIZATION_CODES`, `MAX_FLOWS` and
     `MAX_CEREMONIES` protect one process's memory, and they stay in the
     memory variant. In Redis they are dropped: counting a key space costs
     a scan or a second counter on every insert. The bound is the per-address
     start limits times the replica count, and Redis's `maxmemory`.
   - The per-actor bounds (`lessons.md`, "Bounded state per actor") stay.
     A browser's old flow or ceremony is removed when it starts again, and a
     console session holds one link flow and one registration. In Redis the
     session indexes are keys of their own (`SET ... PX ... GET` swaps the
     pointer atomically and returns the old one), with the same sliding TTL.
9. **MCP sessions use rmcp's `SessionStore`.** `shared::sessions::RedisSessions`
   implements rmcp 3.8's `SessionStore` and is set as
   `StreamableHttpServerConfig::session_store`. rmcp then restores a session
   another instance created by replaying its `initialize`.
   - rmcp logs the session id when `store`, `load` or `delete` returns an
     error (`tower.rs:1707-1712, 2192-2197, 2382-2384`), and a session id is a
     credential. So the implementation never returns an error: it counts
     it, logs it without the id, and answers `Ok(None)` or `Ok(())`. A failed
     `load` fails closed (404, and the client initializes again).
10. **Bindings move with the session, and an unbound session is never
    restored.**
    - The binding (the key id that opened the session) is stored under
      the same hashed session id, with the same TTL as the session's
      state.
    - `bind_session` checks it in Redis on every request, before rmcp
      runs, with one Lua script. The script compares the key id and, only on
      a match, refreshes both keys' TTL to `session_idle_timeout`.
    - The script also requires the state key to exist. An evicted state
      fails closed on every instance, including one that still holds a
      local worker (review of PR #552).
    - A missing binding or another key gets rmcp's 404, as today.
      `RedisSessions::load` also refuses a state without a binding, so a
      restore can't happen even if the middleware were bypassed.
    - A Redis error answers 503, never a fallback to memory.
    - **Binding checks the state exists.** rmcp's `store` swallows its
      errors (Decision 9), so the binding script writes the binding only if
      the session's state is in Redis. It gives both keys the same TTL.
      Otherwise the `initialize` answers 503 and the client starts again,
      instead of holding a session that only the instance that opened it
      can serve (review of PR #552).
    - **`DELETE` works on any instance.** rmcp restores a session only for
      POST and GET. So with Redis, `bind_session` checks the binding and
      releases both keys itself before rmcp runs. It answers 202 even when
      this instance held no local copy. Other instances' local copies are
      refused from then on, because the binding is gone.
11. **The idle TTL is refreshed in `bind_session`.** rmcp calls `store`
    only after `initialize` and `delete` only on `DELETE /mcp`. It never
    refreshes an entry on use, and a session worker that idles out on one
    instance closes only its local copy (`tower.rs:1621-1643`). The
    middleware runs on every request, so it is where the TTL slides.
12. **Key revocation reaches every instance at once: `admin_generation`
    bumps are announced over Redis pub/sub** (the maintainer, 2026-11-17).
    - Channel `<prefix>admin-generation`. Each instance tracks the last
      generation it announced, apart from the one it loaded. An announcing
      reload (the poll, `settle`, the consent approval's poll) publishes
      whenever the loaded generation differs from the announced one,
      whether or not that reload moved it.
      - This matters when a non-announcing reload (an announcement received,
        a resubscribe, a catch-up) loads a local change before the
        `settle` that would announce it. If only a moving reload published,
        that change would never be announced (review of PR #552).
      - The cost is one extra message per instance per bump. Receivers
        already at that generation ignore it.
    - The API's `settle` (which polls after every administrative change),
      the consent approval's poll and the poll loop all go through that
      reload, so the instance that made a change announces it before its
      response returns.
    - Every instance runs a subscriber. On a message it reloads its key
      cache unless it is already at the announced generation.
    - On every (re)subscribe it reloads too, since messages sent while it
      was disconnected are lost. Reconnects back off from 1 s to 30 s.
    - The poll loop stays as the fallback, so a lost announcement costs at
      most `poll_interval`.
    - This covers key revocation, user deletion, team suspension, grant
      revocation and every other change that bumps `admin_generation`.
    - Session bindings are not removed on revocation: `require_key` refuses
      the revoked key on every instance once its cache has reloaded, and
      the bindings expire.
13. **The CLI announces too** (confirmed by the maintainer, 2026-11-17). A CLI command (`okf-mcp key revoke`, `user
    delete`, ...) changes Postgres directly. With `[redis]` set and its
    variable present, every command except `serve` publishes an empty
    announcement after it succeeds. Each instance then checks
    `admin_generation` and reloads only if it moved. A missing variable or
    an unreachable Redis is a warning, not a failure: the change is made,
    and the instances notice it within `poll_interval`.
14. **A grant missed on one instance is caught up before a refusal.** The
    consent approval on A creates the grant and announces it, but the
    client may exchange the code on B before B's subscriber has reloaded.
    - With Redis on, a token exchange, a refresh or an access-token check
      that finds a live row but no grant in the key cache first calls
      `App::catch_up_keys`. It reads `admin_generation` from Postgres and,
      if it has moved, reloads (serialized with the poll).
    - Then it looks again. Only a grant that is still missing is refused.
    - These paths are rate-limited (token) or need a live token, so an
      attacker cannot use the catch-up to make a database read per request.
15. **A revoked access token stops working on every instance: a revocation
    counter in Redis** (the maintainer, 2026-11-17).
    - `AccessTokens::forget` (after a revocation commits) also `INCR`s
      `<prefix>oauth:access-revocations`.
    - With Redis on, every `verify` first reads the counter. If it differs
      from the value this instance last saw, the local cache is cleared, and
      its generation is bumped, so no lookup in flight re-caches.
    - A Redis error skips the cache for that request (a database lookup),
      and caches nothing.
    - As a backstop for a crash between the commit and the `INCR`, or a
      lost counter, entries cached while Redis is on are dropped after
      60 s.
    - The counter has no TTL, and instances compare it with `!=`. A
      counter that restarted at 1 could equal a value an instance has
      already seen, and that instance would keep a revoked token cached
      (review of PR #552). So whoever finds it missing, reader or writer,
      first recreates it inside a Lua script from a random seed below
      2^62. A lost counter therefore never repeats a value an instance has
      seen, and every cache is cleared at its next lookup.
16. **When Redis fails after start-up, auth flows fail closed.**
    - An MCP request answers 503.
    - A consent page, a token exchange, an OIDC callback or a passkey step
      answers 503 or its own "try again" page.
    - Nothing falls back to memory: that would split state between
      instances.
    - The pub/sub subscriber reconnects, and the poll loop covers the gap.
    - Every failure is counted on `okf.redis.errors{operation}` and logged
      with the operation and the error kind, never a key, a value or the
      URL.
17. **Telemetry.**
    - A counter `okf.redis.errors` with a closed `operation` label: `connect`,
      `get`, `set`, `take`, `update`, `remove`, `index`, `admit`, `bind`,
      `publish`, `subscribe`, `counter`, `decode`.
    - A span on each Redis call (`db.system.name = "redis"`,
      `db.operation.name = "<operation>"`), as the Postgres calls have. The
      `hyp_tracing` lint applies.
18. **Tests use a real Redis.** `tests/support/redis.rs` starts a
    `redis:7.4-alpine` container through testcontainers-modules' `redis`
    feature. The module's default tag is 5.0, which has no `GETDEL`.
    - Each test gets its own random `key_prefix`, so a shared server
      (`TEST_REDIS_URL`, like `TEST_PG_URL`) is safe to reuse.
    - The unit tests of the abstraction run against both variants.
    - The two-instance tests run two `App`s, or two `TestServer`s, over one
      Postgres database and one Redis, with `poll_interval = 1h`. Anything
      that passes was carried by Redis, not by the poll.

19. **`/healthz` reports Redis without changing its status** (the
    maintainer, 2026-11-17). The body gains `"redis": "ok" | "unavailable"`,
    or `null` without `[redis]`. It is probed with `PING` beside the database
    probe, under the same deadline. The status code still follows the
    database alone: a Redis outage would otherwise mark every replica unready
    at once and empty the Service.

## Architecture

```
src/shared/
  mod.rs        Redis: the handle (ConnectionManager, prefix, metrics), connect, start-up checks, key names
  ephemeral.rs  Ephemeral<V> and Index: memory or Redis, the envelope, the Lua compare-and-set
  sessions.rs   RedisSessions (rmcp SessionStore) and the binding admit/bind/release scripts
  announce.rs   admin_generation announcements: publish, the subscriber task, the CLI announcement
```

`App` holds `Option<shared::Redis>` (`App::redis`). Every component that has
a Redis variant is built from it in `App::load_with_redis`, so tests can
build two `App`s over one Redis.

### Key layout

`<p>` is `key_prefix`, and `h(x)` is the hex SHA-256 of `x`.

| Key | Value | TTL |
|-----|-------|-----|
| `<p>mcp:state:h(session)` | rmcp `SessionState` (JSON envelope) | `session_idle_timeout`, refreshed on every admitted request |
| `<p>mcp:bind:h(session)` | the key id | as the state |
| `<p>oauth:pending:h(id)` | `PendingAuthorization` | 10 min (`PENDING_TTL`), fixed |
| `<p>oauth:code:<code key>` | `AuthorizationCode` | 60 s, fixed; stays after use for replay detection |
| `<p>oauth:access-revocations` | integer | none |
| `<p>oidc:flow:h(cookie)` | `Flow` | 10 min (`FLOW_TTL`), fixed |
| `<p>oidc:link:<session key>` | `h(flow cookie)` | 10 min, sliding |
| `<p>passkey:ceremony:h(cookie)` | `Ceremony` | 5 min (`CEREMONY_TTL`), fixed |
| `<p>passkey:session:<session key>` | `h(ceremony cookie)` | 5 min, sliding |
| channel `<p>admin-generation` | the generation, or empty | (pub/sub) |

`<code key>` is today's `code_key` (already the hex SHA-256 of the code).
`<session key>` is today's `URL_SAFE_NO_PAD` SHA-256 of the console
session id. The keys are listed in the operations guide.

### Memory with Redis on

Each instance still keeps `LocalSessionManager`'s in-memory session
workers. A session restored on B runs a second `OkfServer` there, and
`okf.mcp.sessions.active` counts it on both instances. The per-instance
caches (catalog snapshots, overlays, the key cache, the access-token
cache) stay in memory; only their invalidation goes through Redis.

## Privacy and security

- **The binding check comes first.** It runs before rmcp, so a request on
  another key's session never reaches the store's `load`.
- **Single use across instances.** Ceremonies and flows use `GETDEL`. Codes
  use the compare-and-set `update`, so two instances racing one code
  produce one exchange and one replay.
- **Every existing check stays.** PKCE, the redirect URI, the client
  binding, expiry, CSRF, the summary and consent nonces, WebAuthn counters,
  and the per-address start limits. Only where the state lives changes.
- **No credential appears in key names, logs or spans** (Decision 7). Redis
  values hold flow secrets for at most 10 minutes. The operations guide
  says Redis must be password-protected, and it recommends TLS
  (`rediss://`) where it crosses hosts.
- **Eviction.** The guide recommends `maxmemory-policy noeviction` or a
  `volatile-*` policy, so the revocation counter (no TTL) is never evicted.
  An evicted entry fails closed in every case: a 404 session, an unknown
  code, an expired flow.

## Docs

- `docs/operations.md` "Why one replica" is rewritten with the real reasons.
  - Without `[redis]`: one replica, because of the in-memory state above.
  - With `[redis]`: N replicas work. The caveats that remain are per-instance
    `GET /mcp` streams, read lag up to `poll_interval`, per-instance rate
    limits (#357), the mail worker (#458), the hourly prune running on
    every instance (idempotent), and `okf.mcp.sessions.active` counting
    restored sessions on each instance.
- A new "Redis" section covers:
  - the config, the key layout and TTLs;
  - what happens when Redis fails;
  - sizing (entry sizes times the start limits);
  - eviction policy;
  - TLS.
- The example Deployment's `replicas: 1` comment names the real reason. It
  also shows the `[redis]` variant with `replicas: 2` and the
  `RollingUpdate` strategy.
- There is no Deployment under `examples/kubernetes/`; the only example
  Deployment is in `docs/operations.md`. The issue's mention of
  `examples/kubernetes` is covered there.
- The comment in `src/catalog.rs:177-182` is already accurate: it calls the
  advisory lock "the other half". It stays.
- The server design (`2026-10-26-okf-mcp-server-design.md`, lines 280-281
  and 1335) gets a dated note pointing here.

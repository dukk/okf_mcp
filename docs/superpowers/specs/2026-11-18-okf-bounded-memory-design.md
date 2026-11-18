# okf-mcp bounded memory: design

Status: designed 2026-11-18; to be delivered in milestone 18. Folds in
issue #458 (HA-tolerant mail worker).

## Goal

A replica's memory, startup time and background database load stop
growing with the total number of users and catalogs. The design target is
about 1M users and 1M catalogs, of which a small fraction is active at
once, served by N identical replicas behind one Service with no session
affinity required.

Concretely:

- **Catalog snapshots are loaded on demand** into a bounded cache, and
  evicted when unused. Startup loads none.
- **Credentials and users are loaded on demand** into bounded caches. A
  change reloads only what it touched, never the whole key cache.
- **Listing catalogs is a query**, not a scan of an in-memory map.
- **Changes reach every replica through targeted invalidations** on the
  existing Redis announce channel, with a TTL backstop on every entry.
- **Cluster-wide housekeeping runs once per cluster** as apalis jobs, not
  on every replica's poll loop.
- **Metric cardinality is bounded**: no series per catalog.

The poll loop is removed.

## Why things grow today

| State | Where | Grows with | Reloaded |
|---|---|---|---|
| Every catalog's snapshot | `App::catalogs` (`src/app.rs`), built by `App::poll_pass` | all catalogs' content | the changed ones, every `poll_interval`; all of them at startup |
| The catalog list | the same map, scanned by `list_catalogs`, `GET /api/catalogs`, `/api/search`, identity, labels | all catalogs | every pass, one row per catalog per replica |
| The key cache | `KeyCache::read` (`src/auth.rs`): every key, grant, user, ceiling, label, suspension and gate | all users and keys | whole, on every `admin_generation` bump anywhere |
| Per-catalog metrics | `okf.catalog.concepts`, `okf.catalog.generation`, `okf.changesets.open`, `okf_catalog_refreshes_total` | all catalogs | every scrape |
| Housekeeping | `prune_*`, `count_open_changesets`, `mail::queue::record_depth` in `App::poll_pass` | replicas | every pass on every replica |

With three replicas this already shows as three times the poll passes and
catalog refresh counts. At 1M catalogs a replica would need the whole
corpus in RAM, and a key created anywhere would reload every user's
credentials on every replica.

## Non-goals

- **Partitioning catalogs across replicas** (routing each catalog to an
  owning replica). Memory per replica is bounded by the active set, not
  divided by N. Partitioning can be layered on later if the active set
  outgrows one replica's budget; the cache metrics below show when.
- **Requiring session affinity.** Everything is correct without it.
  Hashing on `Mcp-Session-Id` at the ingress is documented as an optional
  optimisation (fewer cold loads, roughly 1/N of the active set per
  replica).
- **Moving snapshots or search into Redis or Postgres.** Reads stay on an
  in-process snapshot, and every `okf` analysis is unchanged.
- **Shared rate limits (#357).** Unchanged.
- **Changing the mail retry window or backoff** (the mail queue's
  Decision 11).

## Decisions

### 1. One milestone for catalogs, credentials, metrics and housekeeping

All five sources of growth are fixed together, so the milestone ends with
no per-replica state proportional to users or catalogs.

### 2. Catalog snapshots live in a bounded, weighted cache

`CatalogCache` replaces the `Catalogs` map. It is a moka async `Cache`
keyed by catalog id, whose values are today's `Arc<CatalogState>`,
unchanged inside.

- **Weight** is an estimate of the entry's resident bytes: the sum of its
  stored file sizes times a factor measured during implementation (Task:
  measure on the real catalog and a synthetic large one, record the factor
  and the method in this spec). The cache's capacity is
  `catalog_cache_bytes`.
- **Loads are single-flight** (`try_get_with`): concurrent misses on one
  catalog build it once, through today's `CatalogState::load`.
- **A stale hit refreshes in place** with today's `refresh_to`. Readers
  keep serving the old snapshot until the new one is swapped in.
- **A catalog larger than the whole budget** is loaded, served to the
  requests waiting on it, and not retained; a warning names its size.
- **A catalog whose stored data will not build** is recorded in a small
  negative cache keyed by `(id, generation)`, so that generation is not
  rebuilt on every request. This replaces the poll's `broken` map. The
  request gets today's error.
- **A database error on a miss** is returned to that request and not
  cached.

### 3. A request pins the catalogs it uses

`ToolContext` no longer copies the whole map at construction. It resolves
each catalog the first time the request touches it and keeps the `Arc`
for the rest of the request, so one request sees one snapshot per catalog
even if the entry is evicted or refreshed meanwhile.

### 4. Per-catalog locks move out of the cache entry

The in-process publish mutex (`publishing`) and the rebuild mutex
(`rebuilding`) live in `CatalogState` today. If an entry were evicted and
loaded again, two publishes could hold two different mutexes. Both move to
`KeyedLocks` (`src/keyed_lock.rs`), keyed by catalog id, independent of
cache residency. The advisory lock in the publish transaction is
unchanged.

### 5. Overlays get a byte budget

Overlays (`src/overlay.rs`) each hold a full snapshot. They keep their
own cache, now weighed by the same estimate and bounded by
`overlay_cache_bytes` in place of `overlay_cache_size`, which parses with
a deprecation warning for one release and is then removed.

### 6. The catalog directory answers "exists?" and "which?" without content

`CatalogDirectory` is a bounded TTL cache of catalog rows, keyed by name,
holding what `select_catalogs!` reads (`id`, `name`, `display_name`,
`description`, `generation`, `owner_team`) plus the access facts of
Decision 9 (owner team suspended, maintenance opt-out).

- `resolve(name)` serves the cached row if it is younger than
  `catalog_ttl` (default 10s) and not invalidated, else reads it with one
  indexed query. Its `generation` is what `CatalogCache` checks entries
  against, so freshness has one point of control.
- A missing name is cached as absent for 5s. Creating a catalog
  invalidates the absent entry.
- `description` and `owner_team` are read from the directory, not copied
  into `CatalogState` (`set_description` and `set_owner_team` go away).
- Capacity: `catalog_directory_entries` (default 100,000).

### 7. Listing catalogs is a query driven by the principal's reach

Every place that scans the map and filters by `principal.reaches` becomes
a query:

- `Reach::Catalogs(map)`: `WHERE name = ANY($1)`, bounded by the key's
  grants.
- `Reach::All`: paginated, `ORDER BY name` with an opaque cursor and a
  limit (default 100, max 1000).

Paginated responses gain a `next` cursor (absent on the last page):
`GET /api/catalogs`, the identity endpoint's catalog list, the labels
endpoint's catalog list, and the MCP `list_catalogs` tool. The console
follows the cursor. This is a response shape change and is called out in
the release notes.

### 8. Search fan-out is capped

`GET /api/search` with no catalogs named searches every reachable catalog
today, which for a `Reach::All` key would be every catalog in the system.
It now searches at most `search_fanout_max` catalogs (default 25). A
request that would exceed it is refused with a 400 asking the caller to
name the catalogs. The cap applies to every key. MCP `search` is already
bounded by catalog selection and does not change.

### 9. The key cache is split by what each change touches

| Cache | Key | Holds | Invalidated by |
|---|---|---|---|
| Credential | key hash (API key), grant id (OAuth) | the key or grant row: reach, level, default catalog, flags, user id | `credential` |
| User | `users.id` | admin flag, name, email, ceiling, team ids, entitlements, live unlocks | `user`, `team` (each cached user listing that team) |
| Catalog access facts | catalog name | owner team, suspended, maintenance opt-out | `catalog_meta` (part of the directory row, Decision 6) |
| Labels | slug | display labels | `user`, `team`, `catalog_meta` |

A request builds its principal from the credential entry and the user
entry, capping reach by the ceiling as `principal_of` does today.
Suspension and gate checks (`Principal::require`, `check_open`,
`admission`) look up the catalog being acted on in the directory at the
moment of the check, instead of a map shared by every principal. Unlock
expiry is still checked on every request.

- **TTL backstop:** `credential_ttl`, default 10s, the same bound
  `poll_interval` gives a lost announcement today. Revocation is never
  weaker than now.
- **Unknown credentials are not cached.** A bad token costs one indexed
  query; the existing rate limiters guard guessing. This removes the
  window in which a newly created key is refused on another replica, so
  `App::catch_up_keys` goes away.
- **The OAuth `AccessTokens` cache** (`src/oauth/access.rs`) stays, and
  resolves its grant through the credential cache.
- **Capacity:** `credential_cache_entries` and `user_cache_entries`
  (default 100,000 each). Label lookups share a bounded cache of 100,000
  slugs.
- **Settings** are global and small: loaded once per replica, reloaded on
  a `settings` invalidation or after `credential_ttl`. `admin_generation`
  remains only as the settings version.

### 10. Invalidations travel on the existing announce channel

The Redis pub/sub channel `{prefix}announce` (`src/shared/announce.rs`)
keeps its subscriber task, reconnects and failure metrics. Its payload
becomes versioned JSON:

```json
{"v":1,"kind":"catalog","id":42,"generation":17}
```

Kinds and fields:

| Kind | Fields | Sent on |
|---|---|---|
| `catalog` | `id`, `generation` | publish, import |
| `catalog_meta` | `id`, `name` | create, delete, rename, description, owner, suspension, maintenance opt-out |
| `credential` | `key_id` | key or grant create, edit, revoke |
| `user` | `id` | membership, ceiling, admin flag, entitlement, unlock, profile change |
| `team` | `id` | member access policy, membership, suspension (which also sends `catalog_meta` per owned catalog) |
| `settings` | none | any settings change |

Every write path that commits one of these changes calls one helper,
`Invalidations::send(&[Invalidation])`, **after commit**: the HTTP
handlers, the MCP tools and the CLI (through today's `announce_from_cli`
connection). The sending replica applies the invalidations to its own
caches directly. A `credential` message carries the key id, never the key
hash.

### 11. Loads cannot resurrect stale data

Each cache keeps a per-key invalidation epoch (a small counter map,
bounded with the cache). A load records the epoch when it starts and
inserts its result only if the epoch has not moved; otherwise it serves
the result to its own request and does not cache it. A `catalog` message
also records the announced generation as the catalog's minimum, so a load
that read an older generation is never cached.

### 12. Reconnects mark everything stale

When the subscriber resubscribes after losing Redis, every cached entry is
marked stale (not dropped): each is revalidated against Postgres on next
use. Content stays warm, and no message lost during the gap is trusted.

### 13. Without Redis, one replica, as today

Write paths apply invalidations locally. Changes from the CLI (a separate
process) arrive through the TTLs: 10s for catalogs (`catalog_ttl`) and
credentials (`credential_ttl`), the same bound `poll_interval` gives
today.

### 14. Mixed versions during a rolling deploy are safe

- A new replica that receives an old integer payload treats it as
  `settings` plus a drop of its credential and user caches.
- An old replica that receives a JSON payload cannot parse a generation
  from it, and its existing code reloads the key cache on an unreadable
  payload.

The integer handling is removed one release later.

### 15. Housekeeping runs as apalis jobs, once per cluster

Queue `housekeeping`, one job type per task:

| Task | Cadence | Work |
|---|---|---|
| `prune_changesets` | 5 min | empty changesets unchanged for 24h |
| `prune_oauth` | hourly | expired tokens and codes, unbound clients, old sign-in events, expired emailed links, console sessions, old mail jobs |
| `prune_audit` | daily | entries past `audit_retention_days` plus the margin |
| `prune_workers` | hourly | dead `apalis.workers` rows (#458) |
| `cluster_stats` | 60s | open changesets total, mail queue depth by status |

Scheduling uses a table of ours, leaving apalis's schema untouched:

```sql
CREATE TABLE housekeeping_schedule (
    task        text PRIMARY KEY,
    next_run_at timestamptz NOT NULL,
    job_id      text
);
```

- **Seeding:** at startup each replica runs, per task, in one transaction,
  `INSERT … ON CONFLICT DO NOTHING` and pushes the first job only if its
  insert won. Concurrent starts create one job.
- **Rescheduling:** a job does its work, then in one transaction sets
  `next_run_at` and `job_id` and pushes its successor.
- **Repair:** a sweep on each replica, once a minute, locks each row with
  `FOR UPDATE SKIP LOCKED`; if the task is overdue by more than its grace
  (twice its cadence, at least 2 min) and `job_id` is not `Pending` or
  `Running` in `apalis.jobs`, it pushes a replacement. A killed job, or a
  crash between commit and push, cannot stop a task for good.
- **Failures** are retried by apalis (3 attempts), then logged and left to
  the next scheduled run.

### 16. Database-wide gauges are reported identically by every replica

`cluster_stats` writes its counts to a one-row `cluster_stats` table
(`open_changesets bigint`, `mail_queue jsonb`, `computed_at`). Each
replica refreshes an in-memory copy from that row once a minute and its
gauge callbacks serve the copy, so every replica reports the same value
and dashboards use `max()`. `okf.changesets.open` loses its `catalog`
label and becomes one cluster total.

### 17. Every replica runs a worker (#458)

- `mail.worker` is deprecated: it parses, logs a warning, and is ignored.
  Every `serve` runs one apalis worker for `send_mail` and
  `housekeeping`. Tests disable it with a constructor flag, not config.
- The worker's dedicated pool shrinks from 4 connections to 2, shared by
  both queues. `docs/operations.md` gives the Postgres connection total
  for N replicas: `N × (database_max_connections + 2)`.
- Orphan recovery: heartbeat 10s, 6 missed heartbeats, so a dead pod's
  job returns after about 1 minute. Documented, with the `sending` state
  it implies for mail.
- On SIGTERM the worker stops fetching and waits for its in-flight job up
  to `worker_shutdown_grace` (default 30s), which the docs match to
  `terminationGracePeriodSeconds`. Mail delivery is documented as
  at least once, with the duplicate case spelled out.
- `prune_workers` deletes `apalis.workers` rows whose `last_seen` is older
  than 10 orphan timeouts and that no `Running` job references.

### 18. The poll loop is removed

`App::poll`, `poll_pass`, `poll_until`, the `polling` mutex and
`poll_interval` go. `poll_interval` parses with a deprecation warning for
one release. What remains on each replica is the once-a-minute housekeeping
repair sweep (Decision 15) and the `cluster_stats` refresh (Decision 16).

### 19. Metrics have no per-catalog series

Removed: `okf.poll` (span and duration metric), `okf_catalog_refreshes_total`,
`okf.catalog.concepts`, `okf.catalog.generation`, and the `catalog` label
on `okf.changesets.open`.

Added, labelled by `cache` (`catalog`, `overlay`, `directory`,
`credential`, `user`, `labels`) where it applies:

- `okf.cache.requests{cache,result}` with `result` = `hit`, `miss`,
  `stale`;
- `okf.cache.evictions{cache,cause}` (`size`, `ttl`, `invalidated`);
- `okf.cache.entries{cache}` and `okf.cache.bytes{cache}` (the weighed
  caches);
- `okf.cache.load.duration{cache,outcome}` (histogram);
- `okf.invalidations.sent{kind}`, `okf.invalidations.received{kind}`,
  and `okf.invalidations.lag` (histogram, send to apply, from a send
  timestamp in the payload);
- `okf.housekeeping.runs{task,outcome}` and
  `okf.housekeeping.duration{task}`.

A large catalog's load is still visible per catalog in traces: the
`okf.catalog.load` span keeps its `okf.catalog` attribute.

## Architecture

```
request ──► auth: CredentialCache ─► UserCache ─► Principal
              │                                     │
              ▼                                     ▼
        CatalogDirectory (rows, access facts) ◄── selection / listing (SQL)
              │ generation
              ▼
        CatalogCache (weighed LRU, single-flight) ──► Arc<CatalogState> pinned per request
              ▲
  Redis announce ──► Invalidations::apply (epoch bump, drop / mark stale)
              ▲
  write paths (HTTP, MCP, CLI) ──► Invalidations::send after commit

apalis worker (every replica): send_mail, housekeeping
  housekeeping_schedule ─► one job chain per task ─► cluster_stats row ─► gauges
```

New modules:

| Module | Responsibility |
|---|---|
| `cache::catalogs` | `CatalogCache`: weighed LRU, single-flight load, stale refresh, negative cache |
| `cache::directory` | `CatalogDirectory`: rows by name, absent entries, reach-driven listing queries |
| `cache::credentials` | credential, user and label caches; principal assembly |
| `cache::epoch` | per-key invalidation epochs and minimum generations, shared by every cache |
| `invalidate` | the `Invalidation` enum, its JSON form, `send` and `apply` |
| `housekeeping` | job types, scheduling table, repair sweep, `cluster_stats` |

`App` holds the caches instead of `catalogs`, `keys`, `polling`,
`broken`, `open_changesets`, `oauth_pruned` and `audit_pruned`.

## Configuration

| Setting | Default | Replaces |
|---|---|---|
| `catalog_cache_bytes` | 1 GiB | (load everything) |
| `overlay_cache_bytes` | 256 MiB | `overlay_cache_size` |
| `catalog_directory_entries` | 100,000 | |
| `credential_cache_entries` | 100,000 | |
| `user_cache_entries` | 100,000 | |
| `catalog_ttl` | 10s | `poll_interval` |
| `credential_ttl` | 10s | `poll_interval` |
| `search_fanout_max` | 25 | |
| `worker_shutdown_grace` | 30s | |

Deprecated (parse with a warning for one release): `poll_interval`,
`overlay_cache_size`, `mail.worker`.

## Error handling

- A cache load that fails with `OkfError::Database` is returned to the
  request (503 or the tool error it maps to today) and not cached.
- A catalog that fails to build is cached negatively per generation
  (Decision 2) and returns today's error.
- A failed invalidation publish is counted and logged; the TTL bounds the
  staleness. The write that triggered it still succeeds.
- An unparseable invalidation payload is counted, logged at warn, and
  treated as a reconnect (Decision 12) for safety.
- A housekeeping job failure is retried by apalis, then logged; the chain
  continues at the next cadence (Decision 15).

## Privacy and security

- Revocation bound is unchanged at 10s on a lost message, immediate
  otherwise, on every replica.
- Invalidation payloads carry ids only: no key hashes, names of people,
  emails or catalog content.
- The search fan-out cap (Decision 8) also limits how much work one
  request can cause.
- Not caching unknown credentials keeps a flood of bad tokens out of the
  cache; it is bounded by the rate limiters.

## Testing

- **Unit tests** for each cache: hit, miss, stale refresh, TTL expiry,
  eviction by weight, the over-budget catalog, negative caching, and
  single-flight (many concurrent misses, one load).
- **The epoch race** (Decision 11): a test holds a load open, delivers an
  invalidation, releases the load, and asserts the stale result is served
  once and not cached. A property test (hegel) interleaves loads and
  invalidations and checks the cache never holds a generation below the
  announced minimum.
- **Invalidation coverage:** for every write path in the table of
  Decision 10, a test asserts the right `Invalidation` is sent after
  commit and not on rollback.
- **Two-replica integration tests** (two `App`s, one Postgres, one Redis):
  publish on A is served on B without waiting for a TTL; revoke on A is
  refused on B; a dropped message is covered by the TTL; a reconnect marks
  entries stale.
- **Mixed-version tests** for both payload directions (Decision 14).
- **Housekeeping:** concurrent seeding creates one job per task; a killed
  job is repaired by the sweep; `prune_workers` removes a stale row and
  keeps a live one; SIGTERM waits for an in-flight job.
- **Scale test** (ignored by default, run in the milestone close-out):
  seed 1M catalog rows (tiny catalogs) and 1M users with keys; assert that
  startup loads no catalog, resident memory stays within the configured
  budgets after touching a working set larger than the budget, and
  listing and auth latencies stay within stated bounds.

## Rollout

1. Ship with Redis on and the new defaults. Old and new replicas coexist
   during the rolling deploy (Decision 14).
2. Watch `okf.cache.requests` hit ratios, `okf.cache.bytes`,
   `okf.cache.load.duration` and `okf.invalidations.lag`.
3. Optionally enable ingress hashing on `Mcp-Session-Id` and compare cold
   loads.
4. One release later: remove the integer payload handling and the
   deprecated settings.

## Docs

- `docs/operations.md`: the caches and their settings, sizing for memory
  and Postgres connections with N replicas, optional session hashing, the
  worker and its shutdown grace, at-least-once mail.
- `docs/reference/`: the paginated responses and the search fan-out cap.
- Release notes: removed metrics and their replacements, response shape
  changes, deprecated settings.

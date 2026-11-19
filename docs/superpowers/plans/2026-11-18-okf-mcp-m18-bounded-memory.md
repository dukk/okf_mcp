# OKF MCP Server, Milestone 18 (bounded memory): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

> **Builds on `main` at `2d4ca10`** (PR #643, after v0.22.0).

**Goal:** A replica's memory, startup time and background database load stop growing with the total number of users and catalogs (target: about 1M of each, a small fraction active). Catalogs, credentials, users and labels load on demand into bounded caches; changes reach every replica through `pg_notify` sent by the transaction that makes them; listing is a query; housekeeping runs once per cluster as apalis jobs; the poll loop and per-catalog metric series go.

**Architecture:**
- **`src/invalidate.rs`** owns the change feed: the `Invalidation` enum, `invalidate(conn, &[Invalidation])` (bumps `admin_generation` and runs `pg_notify('okf_invalidate', ..)` in the caller's transaction), and the listener task that applies each message to the caches. It replaces the ~37 `bump_admin_generation(conn)` calls.
- **`src/cache/`** holds the caches, all built on one wrapper, `cache::Bounded<K, V>` (moka `future::Cache` plus per-key invalidation epochs, stale marking and the `okf.cache.*` metrics):
  - `cache::catalogs::CatalogCache`: snapshots by catalog id, weighed in bytes;
  - `cache::directory::CatalogDirectory`: catalog rows by name, with access facts;
  - `cache::credentials`: credential, user and label caches, and principal assembly.
- **`src/housekeeping.rs`** runs the pruning and the cluster gauges as one apalis job type, scheduled by slot-keyed idempotency keys.
- **`App`** holds the caches instead of `catalogs`, `keys`, `polling`, `broken`, `open_changesets`, `oauth_pruned` and `audit_pruned`.

**Tech Stack:** Rust 2021 with the existing axum 0.8 / sqlx 0.9 / rmcp 3.8 / tokio / apalis 1.1 stack. New dependency: `moka = { version = "0.12.15", features = ["future"] }`. sqlx's `PgListener` is already available through `sqlx` (feature `postgres`).

**Spec:** `docs/superpowers/specs/2026-11-18-okf-bounded-memory-design.md` (Decisions 1-20; Decisions 10, 12-15 corrected during planning).

---

## Conventions for every task

- **Branches and PRs.** Branch per task from `main`, one PR per task, linked to its sub-issue. Follow `AGENTS.md` (review rounds, keeping branches current, `tools/agentic/`) and `tools/agentic/brief-common.md`.
- **TDD.** Write the failing test, run it, implement, run it, commit.
- **Gates** run only through `tools/agentic/gate.sh`: `fmt`, `lint`, `test` and `dylint` before every push. While iterating, use `gate.sh narrow <cargo test args>` (one test-name filter per run).
- **Code style.** Comments explain Rust-specific constructs for a Python-expert, Rust-novice reader, as in the existing code.
  - Every public item gets a doc comment with `# Arguments`, `# Returns` and `# Errors`.
  - No `.unwrap()` outside tests.
  - Errors are existing `OkfError` variants: `Database` (503) for a failed load, `Config` at start-up, `BadRequest`-class variants already used by the API for the fan-out cap.
  - Every new function has `#[tracing::instrument(.., skip_all)]` or a reason-bearing `uninstrumented_fn` allow (the `hyp_tracing` lint is at deny).
- **Privacy rules** (review checklist items for every task):
  - an invalidation payload carries ids and generations only: never a key hash, a name of a person, an email, a label or catalog content;
  - never log a credential, a key hash or a session id; errors go through `split_event!` with the full text in the local-only half;
  - metric labels never carry a catalog, user or key: only `cache`, `kind`, `task`, `result`, `cause`, `outcome`.
- **Behaviour is preserved unless the spec says otherwise.** Existing tests keep passing, apart from `.await`s, constructor arguments and the response-shape changes of Decision 7 (Task 10). Where a test asserted poll-loop behaviour, it is rewritten to assert the same outcome through `NOTIFY` and says so in its comment.
- **Two-replica tests** use `TestServer::start_shared` / `sibling` (`tests/support/server.rs:189`, `:209`). From Task 2 on, the harness sets `catalog_ttl` and `credential_ttl` to one hour (until Task 16 removes it, also `poll_interval`), so a pass proves `NOTIFY` carried the change. Every wait is bounded with `tokio::time::timeout` (`lessons.md`).
- **Changelog.** One entry per task, `changelog/<UTC ts>-m18-task-<NN>-<slug>.md`, with `## Added`, `## Changed` and `## Notes`. Take the timestamp from `date -u +%Y-%m-%dT%H-%M-%SZ`. Task 18 also writes the milestone changelog.
- **The code in this plan has not been compiled.** If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.
- **Placeholders only** in tests and docs: `example.com`, `db.example.internal`, `alice`. No real hostnames, registries, names or keys.
- **Commit messages** end with the attribution line the coordinator gives.

## Verified facts the code relies on

### apalis 1.1.1 / apalis-postgres 1.1.0 (as locked)

- `apalis_postgres::queries::push_tasks(conn: &mut E, queue: &str, Vec<PgTask>)` inserts with no `ON CONFLICT`. Migration `20260508093314` adds `UNIQUE INDEX idx_jobs_idempotency_key ON apalis.jobs(job_type, idempotency_key)`: a duplicate key is a unique violation (SQLSTATE `23505`) and aborts the caller's transaction, so push each housekeeping job in its own statement on the pool, not inside a larger transaction. NULL keys never collide.
- `TaskBuilder::idempotency_key(S)`, `run_at_timestamp(u64)`, `max_attempts(usize)`, `build()`.
- One worker serves one `PostgresStorage<Args>` and one queue: housekeeping gets its own worker. `PostgresStorageFactory` shares one LISTEN connection but `unwrap`s on connect, so it is not used.
- `apalis_postgres::Config::default()` sets `missed_heartbeats = 10` (the doc comment says 2; the code says 10). With `HEARTBEAT_INTERVAL = 30s` a dead worker's jobs return after 300 s today.
- `Config::heartbeat_interval(Duration)`, `missed_heartbeats(usize)`, `queue(..)`.
- `Worker::run_until(signal)` stops fetching on the signal and waits for in-flight tasks; fetched-but-unstarted tasks are re-enqueued (`attempts + 1`).
- `apalis.workers`: `id`, `worker_type`, `storage_name`, `layers`, `last_seen`, `started_at`. `apalis.jobs.lock_by` references `apalis.workers(id)`.
- Status values: `Pending`, `Queued`, `Running`, `Done`, `Failed`, `Killed`.

### moka 0.12.15 (in the registry, not yet in the lock)

- Needs `features = ["future"]`.
- `moka::future::Cache::builder().max_capacity(u64).weigher(|&K, &V| -> u32).time_to_live(Duration).eviction_listener(|Arc<K>, V, RemovalCause|).build()`.
- `try_get_with(key, init_future) -> Result<V, Arc<E>>`: concurrent callers for one key share one init.
- `get(&k)`, `insert(k, v)`, `invalidate(&k)`, `invalidate_all()`, `iter()` (sync, `(Arc<K>, V)`), `entry_count()`, `weighted_size()`, `run_pending_tasks()`.
- `RemovalCause::{Expired, Explicit, Replaced, Size}`.
- The weigher returns `u32`: weights are clamped to `u32::MAX` bytes (4 GiB), above any single catalog's budget.

### sqlx 0.9 `PgListener`

- `PgListener::connect_with(&PgPool)`, `listen(channel)`, `try_recv() -> Result<Option<PgNotification>, Error>`.
- `try_recv` returns `Ok(None)` when the connection was lost; with `eager_reconnect` (default `true`) it has already reconnected and re-`LISTEN`ed. Notifications sent while it was disconnected are lost. `Ok(None)` is the "mark everything stale" signal (spec Decision 12).
- `PgNotification::payload() -> &str`.

### okf-mcp on `2d4ca10`

- `App::catalogs()` (`src/app.rs:626`) and `App::catalog(name)` (`:644`) are sync. `load_with_redis` loads every catalog (`:250-261`). `poll_pass` is `:699-789`.
- `ToolContext` (`src/mcp/context.rs:64`) copies the map once at construction (`:93`). `select` (`:295`), `states` (`:320`), `read_set` (`:341`), `one` (`:366`), `report_catalog` (`:656`) are sync; `state` (`:764`) `expect`s a present name; `resolve_changeset` (`:716`) scans the map by id (`:726`).
- Callers of the sync resolvers: `src/mcp/changesets.rs:129, 258`; `src/mcp/maintenance/work.rs:117, 140, 162, 186, 207`; `src/mcp/maintenance/reports.rs:145, 163, 206, 232, 261, 272`; `src/http/api/browse.rs:182, 233, 364, 417`; `src/mcp/search.rs:115`; `src/mcp/documents.rs:384`; `src/mcp/history.rs:124`; `src/mcp/reports.rs:367`.
- Iterations of the map: `src/mcp/catalogs.rs:91`, `src/mcp/changesets.rs:133`, `src/http/api/catalogs.rs:246`, `src/http/api/search.rs:91`, `src/http/api/identity.rs:171`, `src/http/api/labels.rs:122`, `src/telemetry/metrics.rs:493, 510, 525`, `src/http/mod.rs:229` (`/healthz` count), `src/mcp/mod.rs:114` (`catalog_label` existence).
- `CatalogState` (`src/catalog.rs:155`): `publishing` is locked at `src/store/publish.rs:201` and `src/mcp/changesets.rs:592`; `rebuilding` only in `refresh_to` (`src/catalog.rs:415`). Refresh callers: `src/store/publish.rs:303, 368`, `src/http/archives.rs:577`, `src/app.rs:744`.
- `KeyedLocks` (`src/keyed_lock.rs`): `lock(&self, key: i64) -> KeyedGuard<'_>`, cancel-safe, entries removed when unused.
- `Overlays` (`src/overlay.rs:257`): hand-rolled LRU by count (`overlay_cache_size`, default 64, `src/config.rs:48`).
- `catalogs.name` is `UNIQUE` (`migrations/0001_schema.sql:14`).
- `bump_admin_generation(conn)` (`src/store/catalogs.rs:1036`) has 37 non-test call sites, all inside the write's transaction; no SQL trigger bumps it. The per-function list is in Task 2.
- `KeyCache::read` (`src/auth.rs:954`) runs `key_rows::list`, `ceiling::ceilings`, `users::auth_by_id`, `teams::suspended_catalogs`, `Labels::load`, then `gates::load` (`src/store/gates.rs:37`), which first **writes** missing audit rows (`ensure_audit_rows`, `:157`).
- `settle(app)` (`src/http/api/mod.rs:306`) is `app.poll()`; it is called after every administrative HTTP write (41 sites) and `src/oauth/authorize.rs:1141`.
- `serve` (`src/http/mod.rs:651`) spawns the poll loop (`:657`), the M16 announce subscriber (`:660`), the clock check (`:664`) and the mail worker (`:667`, gated by `mail.worker`).
- The latest migration is `0021_oauth_tokens_grant_activity.sql`.

## Decisions taken while planning (2026-11-18)

1. **Milestone 18.** Labels: `epic`, `milestone-18`. Folds in #458 and #356.
2. **Invalidations ride Postgres `NOTIFY`, not Redis** (the maintainer, 2026-11-18). Spec Decisions 10, 12-14 corrected. `invalidate(conn, ..)` replaces `bump_admin_generation(conn)` in every store function, so the CLI is covered with no extra step and nothing is sent on rollback.
3. **Housekeeping uses apalis's idempotency index** instead of a schedule table and repair sweep (spec Decision 15 corrected).
4. **Audit rows are created with their entity**, plus a backfill migration (spec Decision 20, added during planning).
5. **The first task keeps today's caches** and only changes how they learn of changes: the listener's first `apply` reloads the key cache or refreshes one catalog. Each later task swaps in a bounded cache behind the same `apply`. So every PR ships working software and the risky swaps are isolated.
6. **`ToolContext`'s resolution goes async in its own mechanical task** (Task 7) while still backed by the map, so Task 8's cache swap does not also touch 25 call sites.

## Scope

| Task | Delivers |
|---|---|
| 1 | `src/invalidate.rs`: `Invalidation`, JSON payload, `invalidate(conn, ..)`, the listener task, `apply` onto today's caches, reconnect = mark stale; `okf.invalidations.*` metrics |
| 2 | Every `bump_admin_generation` call becomes `invalidate(conn, &[..])` with its kinds; `catalog` invalidations from publish, import and repair; per-function coverage tests; two-replica tests with TTLs and poll at 1h |
| 3 | Audit rows created by the create paths; migration `0022_audit_backfill.sql`; `gates::load` stops writing |
| 4 | `moka`; `cache::Bounded<K, V>` (epochs, stale marking, single-flight, metrics `okf.cache.*`) |
| 5 | `cache::directory::CatalogDirectory` and the store queries behind it (row by name with access facts, list by names, paginated list) |
| 6 | Publish and rebuild locks move from `CatalogState` to `KeyedLocks` on `App` |
| 7 | `ToolContext` resolution async (`select`, `one`, `states`, `read_set`, `report_catalog`, `changeset`), still over the map; all callers `.await` |
| 8 | `cache::catalogs::CatalogCache` replaces the `Catalogs` map: nothing loaded at start-up, per-request pinning, negative cache, weight factor measured; description and owner from the directory |
| 9 | Overlays weighed in bytes (`overlay_cache_bytes`); `overlay_cache_size` deprecated |
| 10 | Paginated catalog listings (`GET /api/v1/catalogs`, identity, labels, MCP `list_catalogs`), `list_changesets` without a scan, the `/api/search` fan-out cap, the console following `next` |
| 11 | `cache::credentials`: credential and user caches replace `KeyCache` for authentication, sessions and OAuth grants; suspension and gates checked against the directory |
| 12 | Labels on demand (`LabelLookup`), replacing `keys().labels()` |
| 13 | Settings on invalidation; `admin_generation` reads reduced to settings; the M16 announcement bridge (spec Decision 14) |
| 14 | #458: worker pool shared and bounded, heartbeat 10 s x 6, `mail.worker` deprecated, `prune_workers`, shutdown grace |
| 15 | `src/housekeeping.rs`: the `housekeeping` queue, the slot ticker, the five tasks, `cluster_stats` table and gauges |
| 16 | The poll loop, `settle`, `poll_interval`, `okf.poll`, `okf_catalog_refreshes_total` and the per-catalog gauges removed; `/healthz` without the catalog count |
| 17 | Docs: `docs/operations.md`, `docs/reference/`, the server design note, release notes draft |
| 18 | Scale test (ignored by default), smoke test, README, milestone changelog |

**Deliberately deferred**

| Item | Where it goes |
|---|---|
| Partitioning catalogs across replicas | not planned; the cache metrics show when it is needed |
| Shared rate limits | #357, unchanged |
| A shared snapshot store | not planned (spec Non-goals) |

## File map

| File | Tasks |
|---|---|
| `Cargo.toml`, `Cargo.lock` | 4 (`moka`) |
| `src/lib.rs` | 1 (`pub mod invalidate;`), 4 (`pub mod cache;`), 15 (`pub mod housekeeping;`) |
| `src/invalidate.rs` | 1 (create), 8, 11, 12, 13 (each moves `apply` onto its cache) |
| `src/cache/mod.rs`, `src/cache/bounded.rs` | 4 (create) |
| `src/cache/directory.rs` | 5 (create), 11 (access facts read by principals) |
| `src/cache/catalogs.rs` | 8 (create) |
| `src/cache/credentials.rs` | 11 (create), 12 |
| `src/cache/labels.rs` | 12 (create) |
| `src/housekeeping.rs` | 15 (create) |
| `src/store/catalogs.rs` | 1 (`bump_admin_generation` becomes private to `invalidate`), 2, 5 (`row_by_name`, `rows_by_names`, `page`) |
| `src/store/{keys,users,teams,ceiling,settings,unlocks,import,repair,publish}.rs`, `src/store/oauth/{codes,grants}.rs` | 2 |
| `src/store/{users,keys,teams}.rs`, `src/store/oauth/{codes,grants}.rs`, `src/store/gates.rs`, `src/store/audit.rs` | 3 |
| `migrations/0022_audit_backfill.sql` | 3 |
| `migrations/0023_cluster_stats.sql` | 15 |
| `src/catalog.rs` | 6 (locks out), 8 (description and owner out) |
| `src/keyed_lock.rs` | 6 (doc only) |
| `src/store/publish.rs`, `src/mcp/changesets.rs` | 6, 8 |
| `src/mcp/context.rs` | 7, 8, 11 |
| `src/mcp/*.rs`, `src/mcp/maintenance/*.rs`, `src/http/api/browse.rs` | 7 (`.await`s) |
| `src/overlay.rs` | 9 |
| `src/http/api/{catalogs,identity,labels,search}.rs`, `src/mcp/catalogs.rs` | 10 |
| `src/http/console/**` (catalog list views) | 10 |
| `src/auth.rs` | 11 (`Keys`/`KeyCache` replaced), 12 |
| `src/http/console_session.rs`, `src/oauth/{access,token,authorize}.rs` | 11 |
| `src/http/api/{audit,people}.rs`, `src/http/archives.rs`, `src/labels/directory.rs` | 12 |
| `src/shared/announce.rs` | 13 (bridge), 16 |
| `src/mail/queue.rs` | 14, 15 (shared worker pool) |
| `src/config.rs` | 8, 9, 10, 11, 14, 16 |
| `src/app.rs` | 1, 6, 8, 11, 13, 15, 16 |
| `src/http/mod.rs` | 1 (spawn listener), 14, 15, 16 |
| `src/http/api/mod.rs` | 16 (`settle` removed) |
| `src/telemetry/metrics.rs` | 1, 4, 15, 16 |
| `src/cli.rs`, `src/cli/*.rs` | 13 (bridge), 16 |
| `tests/support/server.rs` | 2 (TTLs in `serve_app`), 14 (worker on siblings) |
| `tests/multi_replica.rs` | 2, 8, 11, 13, 15 |
| `tests/scale.rs` | 18 (create) |
| `docs/operations.md`, `docs/reference/*.md`, `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` | 17 |
| `docs/superpowers/specs/2026-11-18-okf-bounded-memory-design.md` | 8 (weight factor), any task that corrects it |
| `README.md`, `changelog/*-m18-milestone.md` | 18 |
| `changelog/` | every task |

## Task dependencies

Each line is `task (the tasks it needs)`. A task starts when every task in its parentheses has merged. Tasks on one line can run in parallel.

```
wave 1:  1 ()     3 ()     4 ()     6 ()     7 ()     14 ()
wave 2:  2 (1)    5 (1, 4)    9 (4)    15 (14)
wave 3:  8 (2, 5, 6, 7)    11 (2, 3, 5)
wave 4:  10 (8)    12 (11)    13 (11)
wave 5:  16 (8, 10, 11, 12, 13, 15)
wave 6:  17 (all feature tasks)
wave 7:  18 (all)
```

Why each edge, and the shared files to merge with care:
- **2 needs 1** (`invalidate`). 2 is the wide one: 37 call sites in 14 store files. Nothing else in waves 1-2 edits those store files except 3, which touches the *create* functions of users, keys, teams and grants: 3 adds audit inserts near the top of each, 2 swaps the bump at the end. Keep both.
- **5 needs 1 and 4**: the directory is a `Bounded` cache and registers with `invalidate::apply`.
- **8 needs 2, 5, 6 and 7**: the cache is invalidated by `catalog` messages (2), resolves rows through the directory (5), relies on locks outside the entry (6) and on async resolution (7). 7 and 8 both edit `src/mcp/context.rs`.
- **11 needs 2, 3 and 5**: credential and user messages (2), audit ids read with the entity (3), suspension and gates from the directory (5). 11 and 8 both edit `src/app.rs` (different fields) and `src/mcp/context.rs` (`admission_to` vs resolution): keep both.
- **10 needs 8**: listings read summary fields from the directory and the cache.
- **12 and 13 need 11**: both remove what is left of `KeyCache`.
- **15 needs 14**: the housekeeping worker uses 14's shared worker pool and timing.
- **16 needs every task that stops reading the poll's output.** 17 documents what shipped; 18 is last.

---

### Task 1: The invalidation feed (`src/invalidate.rs`)

Spec Decisions 10, 12. This task changes no write path (Task 2 does) and no cache (later tasks do): `invalidate(conn, ..)` exists and is tested, and the listener applies messages to today's caches.

**Files:**
- Create: `src/invalidate.rs`.
- Modify: `src/lib.rs`, `src/app.rs`, `src/http/mod.rs`, `src/telemetry/metrics.rs`, `src/store/catalogs.rs`.

- [ ] **Step 1: Write the failing tests** (`src/invalidate.rs`, `#[cfg(test)] mod tests`)

```rust
#[test]
fn every_kind_round_trips_through_its_payload() {
    let all = [
        Invalidation::Catalog { id: 42, generation: 17 },
        Invalidation::CatalogMeta { id: 42, name: "notes".into() },
        Invalidation::Credential { key_id: "k_1".to_owned() },
        Invalidation::User { id: "u_1".to_owned() },
        Invalidation::Team { id: "t_1".to_owned() },
        Invalidation::Settings,
    ];
    for invalidation in all {
        let payload = invalidation.to_payload(1_700_000_000_000);
        assert_eq!(Message::parse(&payload).expect("parses").invalidation, invalidation);
    }
}

#[test]
fn a_payload_of_an_unknown_version_or_kind_does_not_parse() {
    assert!(Message::parse(r#"{"v":2,"kind":"catalog","id":1,"generation":1}"#).is_none());
    assert!(Message::parse(r#"{"v":1,"kind":"nope"}"#).is_none());
    assert!(Message::parse("42").is_none());
}

#[tokio::test]
async fn a_committed_invalidation_is_delivered_and_a_rolled_back_one_is_not() {
    let db = TestDb::start_migrated().await;
    let mut listener = PgListener::connect_with(&db.pool).await.unwrap();
    listener.listen(CHANNEL).await.unwrap();

    let mut tx = db.pool.begin().await.unwrap();
    invalidate(&mut tx, &[Invalidation::Settings]).await.unwrap();
    tx.rollback().await.unwrap();
    let mut tx = db.pool.begin().await.unwrap();
    invalidate(&mut tx, &[Invalidation::CatalogMeta { id: 3, name: "c".into() }]).await.unwrap();
    tx.commit().await.unwrap();

    let got = timeout(WAIT, listener.recv()).await.expect("in time").unwrap();
    let message = Message::parse(got.payload()).expect("parses");
    assert_eq!(message.invalidation, Invalidation::CatalogMeta { id: 3, name: "c".into() });
}

#[tokio::test]
async fn invalidate_bumps_admin_generation_once_per_call() {
    let db = TestDb::start_migrated().await;
    let before = catalogs::admin_generation(&db.pool).await.unwrap();
    let mut tx = db.pool.begin().await.unwrap();
    invalidate(&mut tx, &[Invalidation::Settings, Invalidation::User { id: "u".into() }])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(catalogs::admin_generation(&db.pool).await.unwrap(), before + 1);
}
```

And in `src/app.rs` tests:
- `a_catalog_invalidation_refreshes_that_catalog`: publish through the store on the pool directly (not through `App`), call `app.apply(&Message { invalidation: Catalog { id, generation }, .. })`, assert `app.catalog(name).current().generation() == generation`.
- `a_credential_invalidation_reloads_the_key_cache`: revoke a key through the store, `apply` a `Credential`, assert `app.keys().find(&key).is_none()`.
- `a_lost_connection_marks_everything_stale`: `app.apply_reconnect().await` reloads keys and refreshes every loaded catalog (today's caches have no "stale" flag; reloading is today's equivalent).

Run: `tools/agentic/gate.sh narrow invalidate` — expected: fails to compile (`invalidate` not defined).

- [ ] **Step 2: Implement `src/invalidate.rs`**

```rust
//! The change feed (spec Decisions 10 and 12). A store function that
//! changes what a replica caches calls [`invalidate`] inside its own
//! transaction; Postgres delivers the `NOTIFY` to every listening replica
//! when, and only if, the transaction commits. [`listen`] runs on every
//! replica and applies each message to that replica's caches.

/// The `LISTEN` channel.
pub const CHANNEL: &str = "okf_invalidate";

/// The payload version this build writes and reads.
const VERSION: u8 = 1;

/// What changed. Ids only: never a key hash, a person's name or content.
// `serde(tag = "kind")` writes the variant as a `"kind"` field next to its
// own fields, e.g. `{"kind":"catalog","id":1,"generation":2}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Invalidation {
    /// A publish, import or repair moved the catalog to `generation`.
    Catalog { id: i64, generation: i64 },
    /// The catalog row or its access facts changed (create, delete,
    /// details, owner, maintenance, member access, suspension). `name`
    /// (the catalog's slug, an id, not a person's data) lets a replica drop
    /// a cached "no such catalog" entry on create: the id is new, so no
    /// cache can map it back to the name.
    CatalogMeta { id: i64, name: String },
    /// A key or grant (`api_keys.id`) changed.
    Credential { key_id: String },
    /// A user (`users.id`) changed: profile, admin flag, entitlements,
    /// unlocks, or anything that moves their ceiling.
    User { id: String },
    /// A team (`teams.id`, text) changed: members, roles, label.
    Team { id: String },
    /// A server setting changed.
    Settings,
}

/// One received message: what changed, and when it was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// What changed.
    pub invalidation: Invalidation,
    /// The sender's clock at `invalidate`, in Unix milliseconds, for
    /// `okf.invalidations.lag`.
    pub sent_at_ms: i64,
}

// The wire form: the version and send time beside the flattened variant.
#[derive(serde::Serialize, serde::Deserialize)]
struct Wire {
    v: u8,
    at: i64,
    #[serde(flatten)]
    invalidation: Invalidation,
}

impl Invalidation {
    /// This invalidation's `NOTIFY` payload.
    ///
    /// # Arguments
    /// * `sent_at_ms` - the sender's clock, Unix milliseconds.
    ///
    /// # Returns
    /// The JSON payload.
    pub fn to_payload(&self, sent_at_ms: i64) -> String {
        serde_json::to_string(&Wire { v: VERSION, at: sent_at_ms, invalidation: self.clone() })
            .expect("an Invalidation always serializes")
    }
}

impl Message {
    /// Parses a payload; `None` for another version, an unknown kind or
    /// anything else this build cannot read.
    pub fn parse(payload: &str) -> Option<Self> {
        let wire: Wire = serde_json::from_str(payload).ok()?;
        (wire.v == VERSION).then_some(Self { invalidation: wire.invalidation, sent_at_ms: wire.at })
    }
}

/// Records `invalidations` in the caller's transaction: bumps
/// `admin_generation` once and sends one `NOTIFY` per invalidation, which
/// Postgres delivers at commit and drops on rollback.
///
/// # Arguments
/// * `conn` - the writer's transaction (`&mut tx`).
/// * `invalidations` - what the transaction changed; empty does nothing.
///
/// # Errors
/// [`OkfError::Database`]; the caller's transaction then fails as it would
/// for any other statement.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn invalidate(conn: &mut PgConnection, invalidations: &[Invalidation]) -> Result<(), OkfError> {
    if invalidations.is_empty() {
        return Ok(());
    }
    catalogs::bump_admin_generation(&mut *conn).await?;
    let now = Utc::now().timestamp_millis();
    for invalidation in invalidations {
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(CHANNEL)
            .bind(invalidation.to_payload(now))
            .execute(&mut *conn)
            .await?;
        Metrics::global().invalidation_sent(invalidation.kind());
    }
    Ok(())
}
```

`Invalidation::kind(&self) -> &'static str` returns the serde tag (`"catalog"`, ...), for metric labels.

The listener:

```rust
/// Listens on [`CHANNEL`] until `shutdown`, applying each message to
/// `app`. A lost connection (`try_recv` -> `Ok(None)`, already reconnected)
/// marks every cache stale, since messages sent meanwhile are lost (spec
/// Decision 12). A connect failure backs off from 1 s to 30 s.
#[cfg_attr(dylint_lib = "hyp_tracing", allow(uninstrumented_fn, reason = "long-lived loop; each message gets its own root span"))]
pub async fn listen(app: Arc<App>, shutdown: CancellationToken) {
    let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
    loop {
        let mut listener = tokio::select! {
            () = shutdown.cancelled() => return,
            connected = connect(app.pool()) => match connected {
                Ok(listener) => listener,
                Err(error) => { /* split_event!(warn, ...), sleep backoff.next() or return on cancel */ continue; }
            },
        };
        // A fresh connection: anything sent while we had none is lost.
        app.apply_reconnect().await;
        backoff.reset();
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                received = listener.try_recv() => match received {
                    Ok(Some(notification)) => apply_payload(&app, notification.payload()).await,
                    // Lost and already re-established by sqlx.
                    Ok(None) => app.apply_reconnect().await,
                    Err(_) => break, // log, then reconnect from the outer loop
                },
            }
        }
    }
}

async fn apply_payload(app: &App, payload: &str) {
    match Message::parse(payload) {
        Some(message) => {
            Metrics::global().invalidation_received(message.invalidation.kind(), lag_of(&message));
            app.apply(&message).instrument(tracing::info_span!(parent: None, "okf.invalidate.apply", okf.invalidation.kind = message.invalidation.kind())).await;
        }
        // Unreadable: treat as a reconnect, for safety (spec "Error handling").
        None => { Metrics::global().invalidation_unreadable(); app.apply_reconnect().await; }
    }
}
```

`Backoff` is new: `src/shared/announce.rs` has its 1 s to 30 s backoff inline. Extract it to `src/backoff.rs` (`Backoff::new(first, max)`, `next() -> Duration` doubling to `max`, `reset()`) in this task and use it from both.

- [ ] **Step 3: `App::apply` and `App::apply_reconnect` over today's caches** (`src/app.rs`)

```rust
/// Applies one change-feed message to this replica's caches.
pub async fn apply(&self, message: &invalidate::Message) {
    match &message.invalidation {
        Invalidation::Catalog { id, .. } => self.refresh_catalog_by_id(*id).await,
        Invalidation::CatalogMeta { .. }
        | Invalidation::Credential { .. }
        | Invalidation::User { .. }
        | Invalidation::Team { .. }
        | Invalidation::Settings => self.reload_keys_now(false).await,
    }
}

/// After a lost connection: every cache is suspect.
pub async fn apply_reconnect(&self) {
    self.reload_keys_now(false).await;
    for state in self.catalogs().values() {
        if let Err(error) = state.refresh(&self.pool).await { /* split_event!(warn ...) */ }
    }
}
```

`refresh_catalog_by_id` finds the loaded state by id and calls `refresh`; a catalog not loaded is left to the poll (until Task 8). `CatalogMeta` reloading keys is enough today: description and owner are copied in by the poll, which still runs.

- [ ] **Step 4: Metrics** (`src/telemetry/metrics.rs`): `okf.invalidations.sent{kind}` and `okf.invalidations.received{kind}` (counters), `okf.invalidations.unreadable` (counter), `okf.invalidations.lag` (seconds histogram, send to apply; a negative lag from clock skew records 0). Add the new families to `Metrics::FAMILIES`.

- [ ] **Step 5: Spawn the listener** in `serve` (`src/http/mod.rs`, next to the M16 subscriber at `:660`), and await it in the shutdown sequence like the others. It opens one connection from the pool (`PgListener::connect_with(&pool)`), so `docs/operations.md`'s connection count grows by one in Task 17.

- [ ] **Step 6: Run** `tools/agentic/gate.sh narrow invalidate` and `gate.sh narrow app::tests::a_` — expected PASS. Then the full gates.

- [ ] **Step 7: Commit** — `feat: a change feed over Postgres NOTIFY (M18 task 1)`.

---

### Task 2: Every administrative write and publish sends its invalidations

Spec Decision 10's table. After this task every change reaches every replica within one `NOTIFY` round trip, with or without Redis.

**Files:**
- Modify: `src/store/catalogs.rs`, `src/store/ceiling.rs`, `src/store/settings.rs`, `src/store/keys.rs`, `src/store/users.rs`, `src/store/teams.rs`, `src/store/unlocks.rs`, `src/store/oauth/codes.rs`, `src/store/oauth/grants.rs`, `src/store/publish.rs`, `src/store/import.rs`, `src/store/repair.rs`, `tests/support/server.rs`, `tests/multi_replica.rs`.

- [ ] **Step 1: The mapping.** Replace each `bump_admin_generation(&mut *tx)` with `invalidate(&mut tx, &[..])`:

| Store function (current bump line) | Invalidations |
|---|---|
| `catalogs::create` (`catalogs.rs:178`) | `CatalogMeta { id, name }`, `Team { id: owner }` when it has an owner |
| `catalogs::set_owner` (`:325`) | `CatalogMeta { id, name }`, `Team` for the old and the new owner |
| `catalogs::update_details` (`:493`) | `CatalogMeta { id, name }` |
| `catalogs::set_maintenance` (`:730`) | `CatalogMeta { id, name }` |
| `catalogs::delete` (`:882`) | `CatalogMeta { id, name }`, `Team { id: owner }` |
| `ceiling::set_member_access` (`ceiling.rs:345`) | `CatalogMeta { id, name }`, `Team { id: owner }` |
| `ceiling::entitle` (`:446`), `unentitle` (`:502`) | `User { id }` |
| `settings::set` (`settings.rs:821`), `reset` (`:894`) | `Settings` |
| `oauth::codes::redeem` (`codes.rs:672`) | `Credential { key_id: grant id }` |
| `oauth::grants::create_self_issued` (`grants.rs:312`) | `Credential { key_id: grant id }` |
| `keys::create_provisioner` (`keys.rs:468`), `create_with_id` (`:527`), `create_self_issued` (`:725`) | `Credential { key_id }` |
| `keys::set_maintenance_report_with` (`:1103`) | `Credential { key_id }` |
| `keys::revoke_with` (`:1193`) | `Credential { key_id }` |
| `keys::revoke_if_idle` (`:1341`), called by `revoke_idle_grants` | `Credential { key_id }` (new: today it bumps nothing, and the poll's key reload picks the revocation up) |
| `keys::grant_with` (`:1312`), `ungrant_with` (`:1433`) | `Credential { key_id }` |
| `keys::set_user` (`:1582`) | `Credential { key_id }` |
| `users::create` (`users.rs:253`) | `User { id }`, `Team { id: personal team }` |
| `users::set_email` (`:690`), `set_display_name` (`:757`), `set_admin` (`:850`) | `User { id }` |
| `users::delete` (`:1227`) | `User { id }`, `Credential` for each of the user's keys and grants (the function already selects them), `Team` for each team they were in |
| `unlocks::create` (`unlocks.rs:509`), `end` (`:669`) | `User { id: the unlock's user }` |
| `teams::create` (`teams.rs:292`), `set_display_name` (`:715`), `delete` (`:785`) | `Team { id }` |
| `teams::set_member` (`:515`), `remove_member` (`:585`) | `Team { id }`, `User { id: member }` |
| `teams::suspend` (`:897`), `unsuspend` (`:956`) | `Team { id }`, `CatalogMeta { id, name }` for each owned catalog |
| `publish::publish_locked` (`publish.rs:271`, the generation update) | `Catalog { id, generation: new }` |
| `import` (`import.rs:282`), `repair` (`repair.rs:143`) | `Catalog { id, generation: new }` |

Where a function bumps conditionally (`unentitle`, `set_member_access`, `set_maintenance_report_with`, `set_email`, `unsuspend`), it invalidates under the same condition. `bump_admin_generation` becomes `pub(crate)` used only by `invalidate`; grep must show no other caller.

- [ ] **Step 2: Write the coverage tests.** One test per row, in the store module's tests, through a shared helper:

```rust
/// Listens on the change feed, runs `write`, and returns every message
/// delivered within 2 s of its commit (empty if it rolled back).
pub(crate) async fn invalidations_of<F, Fut>(pool: &PgPool, write: F) -> Vec<Invalidation>
where F: FnOnce() -> Fut, Fut: Future<Output = ()>
```

(in `src/invalidate.rs` under `#[cfg(test)]`). Example:

```rust
#[tokio::test]
async fn revoking_a_key_invalidates_that_credential() {
    let db = TestDb::start_migrated().await;
    let key = create_test_key(&db.pool).await;
    let got = invalidations_of(&db.pool, || async {
        keys::revoke_by_id(&db.pool, &key.id, &actor()).await.unwrap();
    }).await;
    assert_eq!(got, [Invalidation::Credential { key_id: key.id.clone() }]);
}
```

Plus `a_refused_write_sends_nothing` for one representative failing write (e.g. revoking an unknown key) asserting an empty list.

- [ ] **Step 3: Run each new test** (`gate.sh narrow invalidations_of` won't match; run per module, e.g. `gate.sh narrow store::keys::tests::`) — expected FAIL before Step 1's edits, PASS after.

- [ ] **Step 4: Two-replica tests** (`tests/multi_replica.rs`). `serve_app` gains `catalog_ttl` / `credential_ttl` forced to 1 h (the fields arrive in Tasks 8 and 11; until then only `poll_interval = 1h`, already set). New tests, each ending in `eventually(..)` on B:
  - `a_publish_on_one_replica_is_served_on_the_other`: open a changeset and publish on A via MCP; B's `get_concept` returns the new text.
  - `a_key_revoked_through_the_store_is_refused_on_the_other_replica`: revoke with `keys::revoke_by_id` on the pool, as the CLI does (no `settle`, no Redis announcement): B refuses the key. This is the case the M16 design could not cover without the poll.
  - `a_killed_listener_connection_reloads_on_reconnect`: `SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE query LIKE 'LISTEN%'` for B's listener, revoke a key on A, assert B refuses it.

- [ ] **Step 5: Full gates; commit** — `feat: every administrative write and publish sends invalidations (M18 task 2)`.

---

### Task 3: Audit rows are created with their entity (spec Decision 20)

**Files:**
- Create: `migrations/0022_audit_backfill.sql`.
- Modify: `src/store/users.rs` (`create`), `src/store/keys.rs` (`create_provisioner`, `create_with_id`, `create_self_issued`), `src/store/oauth/codes.rs` (`redeem`), `src/store/oauth/grants.rs` (`create_self_issued`), `src/store/teams.rs` (`create`), `src/store/catalogs.rs` (`set_owner`, for the owner team's audit row), `src/store/gates.rs`, `src/store/audit.rs`.

- [ ] **Step 1: Failing tests** (`src/store/gates.rs` tests):
  - `every_new_user_key_grant_and_team_has_its_audit_row`: create one of each through the store functions; assert `audit_people`, `audit_credentials` and `audit_teams` each have the matching row **before** any `gates::load`.
  - `gates_load_writes_nothing`: snapshot row counts of the three audit tables, call `gates::load`, assert unchanged.
  - `the_backfill_creates_missing_rows_and_is_idempotent` (in `src/db.rs` tests, alongside the migration tests): delete a user's audit row, run the migration's SQL, assert it is back; run again, assert no duplicates.

- [ ] **Step 2: The migration** (`migrations/0022_audit_backfill.sql`). Read `src/store/gates.rs:157` (`ensure_audit_rows`) and `src/store/audit.rs` (`audit_person`, `audit_credential`, `audit_team_ref`) first and reproduce exactly the rows they would create, as `INSERT ... SELECT ... WHERE NOT EXISTS` statements, one per table. Copy the column list and any derived value (for example a label snapshot) from those functions; do not invent columns.

- [ ] **Step 3: Create paths.** Each create function calls the matching existing helper (`audit::audit_person(&mut tx, ..)`, `audit_credential`, `audit_team_ref`) in its own transaction, after the entity insert.

- [ ] **Step 4: `gates::load`** drops `ensure_audit_rows` (and the function itself if nothing else calls it).

- [ ] **Step 5: Run, full gates, commit** — `feat: create audit rows with their entity, backfill the rest (M18 task 3)`.

---

### Task 4: `cache::Bounded<K, V>`

Spec Decisions 2 (single flight, weight), 11 (epochs), 12 (stale), 19 (`okf.cache.*`).

**Files:**
- Create: `src/cache/mod.rs`, `src/cache/bounded.rs`.
- Modify: `Cargo.toml` (`moka = { version = "0.12.15", features = ["future"] }`, with a comment like the others), `src/lib.rs`, `src/telemetry/metrics.rs`.

- [ ] **Step 1: Failing tests** (`src/cache/bounded.rs`):

```rust
#[tokio::test]
async fn concurrent_misses_load_once() {
    let cache = Bounded::<i64, Arc<str>>::new(CacheName::Directory, Limit::Entries(10), TTL);
    let loads = AtomicUsize::new(0);
    let load = || async { loads.fetch_add(1, SeqCst); tokio::time::sleep(MS_50).await; Ok::<_, OkfError>(Arc::from("v")) };
    let (a, b) = tokio::join!(cache.get_or_load(1, load), cache.get_or_load(1, load));
    assert_eq!((a.unwrap(), b.unwrap()), (Arc::from("v"), Arc::from("v")));
    assert_eq!(loads.load(SeqCst), 1);
}

#[tokio::test]
async fn a_load_overtaken_by_an_invalidation_is_served_but_not_kept() {
    let cache = Bounded::<i64, i64>::new(CacheName::Directory, Limit::Entries(10), TTL);
    let (release, wait) = tokio::sync::oneshot::channel::<()>();
    let pending = cache.get_or_load(1, || async { wait.await.ok(); Ok::<_, OkfError>(16) });
    let invalidate = async { cache.invalidate(&1).await; release.send(()).ok(); };
    let (served, ()) = tokio::join!(pending, invalidate);
    assert_eq!(served.unwrap(), 16, "the caller still gets what it loaded");
    assert_eq!(cache.peek(&1).await, None, "but it is not cached");
}

#[tokio::test]
async fn invalidating_keys_that_are_not_cached_keeps_no_state() {
    let cache: Bounded<i64, u32> = Bounded::new(CacheName::User, Limit::Entries(10), TTL);
    for key in 0..100_000 {
        cache.invalidate(&key).await;
    }
    assert_eq!(cache.tracked_epochs(), 0, "no load in flight, so no epoch kept");
}

#[tokio::test]
async fn an_epoch_outlives_the_entry_until_the_older_load_finishes() {
    // Load A starts; the key is invalidated; load A is still running when a
    // second invalidation arrives. A must still not be cached when it ends,
    // and the epoch entry is gone once it has.
    /* hold load A on a oneshot, invalidate twice, release, assert peek is None
       and tracked_epochs() == 0 */
}

#[tokio::test]
async fn a_stale_entry_is_revalidated_on_next_use() { /* insert, mark_all_stale(), get_or_revalidate calls `check` once and keeps the value when `check` says it is current */ }

#[tokio::test]
async fn an_entry_heavier_than_the_whole_budget_is_served_and_not_kept() { /* Limit::Bytes(100), weigher says 1000 */ }

#[tokio::test]
async fn entries_expire_after_the_ttl() { /* tokio::time::pause(); advance past TTL; next get reloads */ }
```

- [ ] **Step 2: Implement.** The public surface:

```rust
/// Which cache, for metric labels and logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheName { Catalog, Overlay, Directory, Credential, User, Labels }

/// A cache's capacity: by entry count, or by estimated bytes through a weigher.
#[derive(Debug, Clone, Copy)]
pub enum Limit { Entries(u64), Bytes(u64) }

/// A bounded async cache with single-flight loads, per-key invalidation
/// epochs (a load overtaken by an invalidation is not kept) and stale
/// marking (spec Decisions 2, 11, 12).
pub struct Bounded<K, V> { /* moka::future::Cache<K, Entry<V>>, epochs: std::sync::Mutex<HashMap<K, u64>>, stale_before: AtomicU64, name, limit */ }

impl<K: Hash + Eq + Clone + Send + Sync + 'static, V: Clone + Send + Sync + 'static> Bounded<K, V> {
    pub fn new(name: CacheName, limit: Limit, ttl: Duration) -> Self;
    /// With `Limit::Bytes`: how much one value weighs.
    pub fn with_weigher(self, weigher: impl Fn(&K, &V) -> u32 + Send + Sync + 'static) -> Self;
    /// The cached value, loading it once however many callers miss together.
    pub async fn get_or_load<F, Fut>(&self, key: K, load: F) -> Result<V, OkfError>
    where F: FnOnce() -> Fut, Fut: Future<Output = Result<V, OkfError>>;
    /// As `get_or_load`, and a cached entry marked stale (or older than the
    /// TTL) is first passed to `check`, which returns `Ok(None)` to keep it
    /// or `Ok(Some(new))` to replace it.
    pub async fn get_or_revalidate<F, Fut, C, CFut>(&self, key: K, load: F, check: C) -> Result<V, OkfError>;
    /// The cached value without loading or counting a request (tests, listings).
    pub async fn peek(&self, key: &K) -> Option<V>;
    /// Drops `key` and bumps its epoch.
    pub async fn invalidate(&self, key: &K);
    /// Drops every entry `predicate` selects, bumping their epochs.
    pub fn invalidate_where(&self, predicate: impl Fn(&K, &V) -> bool);
    /// Marks every current entry stale (Decision 12).
    pub fn mark_all_stale(&self);
    /// Re-inserts `key`'s cached value so the weigher runs again: moka
    /// weighs on insert and replace only, not when data behind an `Arc`
    /// changes. Respects epochs like a load (a re-weigh overtaken by an
    /// invalidation is dropped). No-op if `key` is not cached.
    pub async fn reweigh(&self, key: &K);
    /// Keys with a load in flight (tests: the epoch map's size).
    #[cfg(test)]
    pub fn tracked_epochs(&self) -> usize;
}
```

Implementation notes for the engineer:
- Each `Entry<V>` stores `value`, `loaded_at: Instant` and `stamp: u64` (the `stale_before` counter at load). `mark_all_stale` increments `stale_before`; an entry with `stamp < stale_before` is stale.
- Epochs exist only for keys with a load in flight. The map is `inflight: std::sync::Mutex<HashMap<K, InFlight>>`, `InFlight { loads: u32, epoch: u64 }`. Before calling `load`, a caller registers (`loads += 1`, inserting `{ loads: 1, epoch: 0 }` if absent) and records the epoch it saw; a guard deregisters it when the load finishes, errors or is cancelled (`Drop`), removing the entry when `loads` reaches 0. `invalidate(key)` drops the cache entry and, **only if `inflight` has the key**, increments its epoch; an invalidation of a key that is neither cached nor loading leaves no state at all. Use moka's `try_get_with` for single flight, with the init future returning the value and the epoch it started at; after it resolves, if the key's epoch moved, call `cache.invalidate(&key)` and return the value to the callers. The map is therefore bounded by the number of concurrent loads, not by the cache or by the change history. An epoch is never reset while an older load is still registered, because the entry outlives every registered load.
- `Bounded` has no eviction listener for epochs (the listener only counts evictions for metrics).
- Minimum generations (Decision 11, used by Task 8) follow the same rule: `InFlight` also carries `min_generation: Option<i64>`, raised by a `Catalog` message only while a load is in flight. A load that starts after the message was applied reads after the writer's commit (Postgres delivers `NOTIFY` at commit), so it needs no minimum.
- `invalidate_where` uses `iter()` (sync) and calls `invalidate` for each match; the cache is bounded, so the scan is bounded. Do not enable moka's `support_invalidation_closures` (it keeps predicates around).
- Over-budget values: with `Limit::Bytes(n)`, a value whose weight exceeds `n` is returned to the callers of that load and then invalidated, so it is never kept. Concurrent callers still share the one load (moka's single flight); a later request loads it again. Log a warn with the weight, at most once per key per hour.
- Metrics (`src/telemetry/metrics.rs`): `okf.cache.requests{cache,result=hit|miss|stale}`, `okf.cache.evictions{cache,cause=size|ttl|invalidated}` from the eviction listener (`RemovalCause::Size` -> `size`, `Expired` -> `ttl`, `Explicit` -> `invalidated`, `Replaced` not counted), `okf.cache.entries{cache}` and `okf.cache.bytes{cache}` as observable gauges over `entry_count()`/`weighted_size()`, `okf.cache.load.duration{cache,outcome}`.

- [ ] **Step 3: Run, full gates, commit** — `feat: a bounded cache with epochs and stale marking (M18 task 4)`.

---

### Task 5: The catalog directory (spec Decisions 6, 7 store side)

**Files:**
- Create: `src/cache/directory.rs`.
- Modify: `src/store/catalogs.rs`, `src/app.rs` (field and constructor only), `src/invalidate.rs` (`CatalogMeta` and `Catalog` also invalidate the directory), `src/config.rs` (`catalog_directory_entries`, `catalog_ttl`).

- [ ] **Step 1: Store queries** (`src/store/catalogs.rs`). One row type with everything a request needs about a catalog without its content:

```rust
/// A catalog's row with its access facts (spec Decisions 6 and 9).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CatalogRow {
    pub id: i64,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub generation: i64,
    pub owner_team: Option<String>,
    pub owner_team_id: Option<String>,
    /// The owner team's suspension, if suspended (`teams.suspended_at`).
    pub suspended: Option<SuspendedRow>,
    /// `NOT catalogs.maintenance`.
    pub maintenance_off: bool,
    /// The owner team's `audit_teams.id` (`CatalogScope::team`).
    pub audit_team: Option<i64>,
}

pub async fn row_by_name(executor: impl PgExecutor<'_>, name: &str) -> Result<Option<CatalogRow>, OkfError>;
pub async fn rows_by_names(executor: impl PgExecutor<'_>, names: &[String]) -> Result<Vec<CatalogRow>, OkfError>;
/// One page, by name, after `after` (exclusive).
pub async fn page(executor: impl PgExecutor<'_>, after: Option<&str>, limit: i64) -> Result<Vec<CatalogRow>, OkfError>;
```

Build the SELECT once in a macro next to `select_catalogs!`, joining `teams` for the owner and suspension and `audit_teams` for the audit id, copying the column expressions from `teams::suspended_catalogs` (`src/store/teams.rs:974`) and `gates::load`'s catalogs query (`src/store/gates.rs`) so the values are identical to today's. Tests: each function against fixtures with a suspended owner, a maintenance-off catalog and an unowned one; `page` walks three pages of five catalogs in name order.

- [ ] **Step 2: Failing tests** (`src/cache/directory.rs`):
  - `resolve_reads_once_then_serves_from_cache` (count queries with a test hook or by checking `okf.cache.requests` through a test meter).
  - `an_unknown_name_is_remembered_briefly_and_forgotten_by_create`: `resolve("x")` is `None`; create `x` through the store (which sends `CatalogMeta { id, name: "x" }`); after `apply`, `resolve("x")` is `Some`. The directory drops by `name` (rows and absent entries are keyed by name) and by `id` (a cached row with that id, for a delete or an update).
  - `a_catalog_meta_invalidation_drops_the_row` and `a_catalog_invalidation_raises_the_minimum_generation`: after `Catalog { id, generation: 9 }`, a row read at generation 8 is not cached (Decision 11).

- [ ] **Step 3: Implement.**

```rust
/// Catalog rows by name, with absent names remembered for 5 s (spec
/// Decision 6). The generation in a row is what [`CatalogCache`] checks
/// its snapshots against.
pub struct CatalogDirectory {
    rows: Bounded<String, Option<Arc<CatalogRow>>>,
    // id -> name, so an invalidation by id finds the row. Bounded with `rows`.
    names: std::sync::Mutex<HashMap<i64, String>>,
    // id -> the lowest generation a row may have (Decision 11).
    minimum: std::sync::Mutex<HashMap<i64, i64>>,
    pool: PgPool,
}

impl CatalogDirectory {
    pub fn new(pool: PgPool, entries: u64, ttl: Duration) -> Self;
    pub async fn resolve(&self, name: &str) -> Result<Option<Arc<CatalogRow>>, OkfError>;
    /// Rows for `names`, from the cache where present, the rest in one query.
    pub async fn resolve_many(&self, names: &[String]) -> Result<Vec<Arc<CatalogRow>>, OkfError>;
    pub async fn apply(&self, invalidation: &Invalidation);
    pub fn mark_all_stale(&self);
}
```

Absent entries: store `None` with a 5 s lifetime by checking `loaded_at` in `get_or_revalidate`'s `check` (absent entries older than 5 s are reloaded; present ones follow `catalog_ttl`).

- [ ] **Step 4: Wire into `App`** (`App::directory() -> &CatalogDirectory`) and into `App::apply` / `apply_reconnect`. Nothing reads it yet except tests.

- [ ] **Step 5: Run, full gates, commit** — `feat: the catalog directory (M18 task 5)`.

---

### Task 6: Per-catalog locks outside the snapshot entry (spec Decision 4)

**Files:**
- Modify: `src/catalog.rs`, `src/app.rs`, `src/store/publish.rs`, `src/mcp/changesets.rs`.

- [ ] **Step 1: Failing test** (`src/store/publish.rs` tests): `publishes_of_one_catalog_serialize_across_two_states`: build two separate `CatalogState`s for the same catalog (as an eviction and reload would), start two publishes through each, and assert they never overlap (record entry and exit in a shared `Mutex<Vec<_>>`). Today this fails: each state has its own `publishing` mutex.

- [ ] **Step 2: Implement.** `App` gains `publish_locks: KeyedLocks` and `rebuild_locks: KeyedLocks`. `CatalogState::publish_lock()` is removed; callers use `app.publish_lock(state.id()).await` (returns `KeyedGuard<'_>`). `refresh_to` takes the rebuild lock from a `&KeyedLocks` argument passed by its callers (`refresh`, `refresh_locked`), so `CatalogState` holds no mutex. Update the doc comments at `src/catalog.rs:177` and `src/overlay.rs` that describe the in-process half of the publish lock.

- [ ] **Step 3: Run** `gate.sh narrow publish` and `gate.sh narrow lock_order` (`src/mcp/changesets/lock_order_tests.rs` must pass unchanged); full gates; commit — `refactor: per-catalog locks outside the snapshot (M18 task 6)`.

---

### Task 7: `ToolContext` resolves catalogs asynchronously

Mechanical: behaviour unchanged, still over the copied map. It moves the `.await`s now so Task 8 changes only the source.

**Files:**
- Modify: `src/mcp/context.rs`, `src/selection.rs` (only if `resolve`'s `exists` closure must become a precomputed set), and every caller listed in Verified facts.

- [ ] **Step 1:** Change the signatures:

```rust
pub async fn select(&self, argument: Option<&[String]>) -> Result<Selection, OkfError>;
pub async fn check_connection(&self) -> Result<(), OkfError>;
pub async fn states(&self, argument: Option<&[String]>) -> Result<Vec<Arc<CatalogState>>, OkfError>;
pub async fn read_set(&self, catalog: Option<&str>) -> Result<Vec<Arc<CatalogState>>, OkfError>;
pub async fn one(&self, catalog: Option<&str>) -> Result<Arc<CatalogState>, OkfError>;
pub async fn report_catalog(&self, catalog: Option<&str>, pinned: Option<&Pinned>) -> Result<Arc<CatalogState>, OkfError>;
async fn state(&self, name: &str) -> Result<Arc<CatalogState>, OkfError>; // no more `expect`
```

`selection::resolve` keeps a sync `exists` closure: `select` first collects the candidate names (connection list, argument, key default, server default), checks their existence in one step (here: the map; in Task 8: `directory.resolve_many`), then passes a `HashSet` to `resolve`. `state` returns `CatalogNotSelected` instead of panicking for a missing name.

- [ ] **Step 2:** Add `.await` at every caller. `gate.sh lint` lists them; no other change.

- [ ] **Step 3:** Full gates (the existing tests are the test); commit — `refactor: async catalog resolution in ToolContext (M18 task 7)`.

---

### Task 8: `CatalogCache` replaces the catalog map (spec Decisions 2, 3)

**Files:**
- Create: `src/cache/catalogs.rs`.
- Modify: `src/app.rs`, `src/catalog.rs`, `src/mcp/context.rs`, `src/mcp/mod.rs`, `src/store/publish.rs`, `src/http/archives.rs`, `src/invalidate.rs`, `src/config.rs`, `src/telemetry/metrics.rs`, `tests/multi_replica.rs`, the spec (weight factor).

- [ ] **Step 1: Measure the weight factor.** Measure, don't guess. In an `#[ignore]` test `measure_snapshot_overhead`, build snapshots of (a) a 500-document synthetic catalog with links and frontmatter, (b) a 5,000-document one, and measure the allocator delta with a counting `#[global_allocator]` wrapper in that test binary only (`tests/snapshot_weight.rs`, its own binary so the allocator affects nothing else). Report `resident / stored_bytes` for each; pick the larger, round up, and record method and numbers in spec Decision 2 ("Corrected (issue #N)"). The production weigher is `stored_bytes * FACTOR`, clamped to `u32::MAX`.

- [ ] **Step 2: Failing tests** (`src/cache/catalogs.rs`, `src/app.rs`):
  - `start_up_loads_no_catalog`: `App::load` with three catalogs; `okf.cache.entries{cache=catalog}` is 0.
  - `the_first_read_loads_and_the_second_hits`.
  - `a_catalog_invalidation_refreshes_in_place`: readers that pinned the old `Arc<Loaded>` keep it; new reads get the new generation.
  - `a_broken_generation_is_not_rebuilt_on_every_request`: store rows that fail to build; two reads, one build attempt; after a new generation, one more attempt.
  - `a_request_keeps_its_snapshot_while_the_entry_is_evicted` (budget of one catalog, two catalogs read in one `ToolContext`).
  - `a_request_sees_one_snapshot_when_the_catalog_refreshes_between_reads`: in one `ToolContext`, read catalog `a`, publish `a` through the store and `apply` its `Catalog` message (an in-place refresh), read `a` again: both reads return the first generation; a new `ToolContext` sees the second.
  - `a_resident_catalog_that_grows_past_the_budget_is_not_kept`: budget of 10 KiB, load a 2 KiB catalog, publish documents that take it past 10 KiB, refresh it (once through `apply`, once through the publisher's direct refresh): `okf.cache.bytes{cache=catalog}` reflects the new weight and the entry is gone (over budget), not reported at its original size.
  - In `tests/multi_replica.rs`: `a_catalog_created_on_one_replica_is_readable_on_the_other` (no poll: directory invalidation plus load on demand).

- [ ] **Step 3: Implement.**

```rust
/// Catalog snapshots by id, bounded in estimated bytes (spec Decision 2).
pub struct CatalogCache {
    states: Bounded<i64, Arc<CatalogState>>,
    // (id, generation) pairs whose stored data would not build.
    broken: Bounded<(i64, i64), Arc<OkfError>>,
    pool: PgPool,
}

impl CatalogCache {
    pub fn new(pool: PgPool, budget_bytes: u64, ttl: Duration) -> Self;
    /// The state of `row`'s catalog at `row.generation` or newer: cached,
    /// refreshed in place, or loaded.
    pub async fn get(&self, row: &CatalogRow, rebuild_locks: &KeyedLocks) -> Result<Arc<CatalogState>, OkfError>;
    pub async fn apply(&self, invalidation: &Invalidation, rebuild_locks: &KeyedLocks);
    pub fn mark_all_stale(&self);
}
```

`get`: `states.get_or_revalidate(row.id, load, check)`, where `check` compares `state.current().generation()` with `row.generation` and refreshes in place when behind, then returns `Ok(Some(Arc::clone(&state)))`, so `Bounded` replaces the entry with the same `Arc` and the weigher runs on the new snapshot. Every other in-place refresh (the `Catalog` message in `apply`, the publisher's and the import's direct refreshes) ends with `states.reweigh(&id)`, through one `CatalogCache::refreshed(id)` helper so no refresh path can skip it. A delete (`CatalogMeta` whose directory row is now absent) invalidates the id. `CatalogState` loses `description`, `owner_team`, `set_description`, `set_owner_team`: readers take them from the `CatalogRow`. `CatalogState::load` takes `(pool, &CatalogRow)` instead of `(pool, name)`.

`App`:
- `catalogs`, `catalogs()`, `catalog()` and the `Broken` map are removed. New: `App::catalog_row(name) -> Result<Option<Arc<CatalogRow>>>` (directory) and `App::catalog_state(&CatalogRow) -> Result<Arc<CatalogState>>` (cache).
- `load_with_redis` no longer lists or loads catalogs; it checks `default_catalog` with one `directory.resolve`.
- `poll_pass` drops the catalog loop (the rest of the poll stays until Task 16).

`ToolContext`: drops the `catalogs` field; adds `pinned: Mutex<HashMap<String, Pinned>>`, `Pinned { row: Arc<CatalogRow>, state: Arc<CatalogState>, snapshot: Arc<Loaded> }`. Pinning a `CatalogState` alone does not pin a snapshot: `current` is a `RwLock<Arc<Loaded>>` swapped by every in-place refresh. On first use of a catalog, `ToolContext` resolves the row and state and takes `state.current()` once, as `snapshot`. Request reads go through `ToolContext::snapshot(name) -> Arc<Loaded>`; `state(name)` remains for writers that must see the latest (publish under its lock, overlay builds keyed by changeset, which validate against the current generation themselves). Convert the read sites that call `.current()` on a context's state (`grep -rn '\.current()' src/mcp src/http`, about 60) to `snapshot`; any that must stay on `state().current()` gets a one-line comment saying why. `resolve_changeset` finds the catalog by `row.catalog_id` through a new `catalogs::row_by_id` (add to Task 5's queries here) instead of scanning. `ToolContext::catalogs()` is removed; its callers are Task 10's (listings) and the tests.

`src/mcp/mod.rs:114` (`catalog_label` existence for telemetry): use `directory.resolve`.

Publish (`src/store/publish.rs:303`) and HTTP import (`src/http/archives.rs:576`) keep refreshing the publisher's own entry directly (its `NOTIFY` arrives a moment later and is then a no-op).

- [ ] **Step 4: Run, full gates; update the spec's Decision 2 with the factor; commit** — `feat: catalogs load on demand into a bounded cache (M18 task 8)`.

---

### Task 9: Overlays weighed in bytes (spec Decision 5)

**Files:** Modify `src/overlay.rs`, `src/config.rs`, `src/app.rs`.

- [ ] **Step 1: Failing test:** `overlays_are_evicted_by_bytes_not_count`: `overlay_cache_bytes` fits two overlays of a test catalog; build three; the least recently used is gone.
- [ ] **Step 2: Implement.** Replace the hand-rolled `HashMap` and `capacity` with `Bounded<i64 /* changeset id */, Arc<Overlay>>` (`CacheName::Overlay`, `Limit::Bytes`, `time_to_idle = session_idle_timeout` through a `Bounded::with_idle(Duration)` builder method added here), weighed with Task 8's weigher on the overlay's workspace. Keep the validity check (`built_at`) and `builds: KeyedLocks` as they are. Config: `overlay_cache_bytes` (default 256 MiB); `overlay_cache_size` still parses and logs `warn!("overlay_cache_size is deprecated and ignored; set overlay_cache_bytes")` at start-up.
- [ ] **Step 3: Run, gates, commit** — `feat: overlays bounded in bytes (M18 task 9)`.

---

### Task 10: Listings are queries; the search fan-out cap (spec Decisions 7, 8)

**Files:** Modify `src/http/api/catalogs.rs`, `src/http/api/identity.rs`, `src/http/api/labels.rs`, `src/http/api/search.rs`, `src/mcp/catalogs.rs`, `src/mcp/changesets.rs`, `src/config.rs`, the console views that list catalogs (find them with `grep -rn "api/v1/catalogs\|catalogs\"" src/http/console`), `docs/reference/` (Task 17 finishes the prose).

- [ ] **Step 1: One helper** (`src/cache/directory.rs`):

```rust
/// The catalogs `principal` reaches, one page at a time (spec Decision 7).
/// `Reach::Catalogs` returns them all in one page (bounded by the grants);
/// `Reach::All` pages through every catalog by name.
pub async fn reachable_page(&self, principal: &Principal, after: Option<&str>, limit: u32) -> Result<Page<Arc<CatalogRow>>, OkfError>;

pub struct Page<T> { pub items: Vec<T>, pub next: Option<String> }
```

`next` is the last name of a full page, opaque to clients (base64url of the name, so a client cannot depend on it being a name).

- [ ] **Step 2: Failing tests**, one per endpoint: with an all-reach admin key and 7 catalogs, `?limit=3` returns 3, 3, 1 with `next` set, set, absent; a `Reach::Catalogs` key gets its catalogs in one page and no `next`. `CatalogSummary` fields are unchanged; `files`, `concepts` and `last_change` (which need a snapshot) are read through `catalog_state` for each listed catalog, which loads at most `limit` catalogs. `/api/search` with 26 reachable catalogs and none named answers 400 with `"name the catalogs to search (at most 25 are searched by default)"`; with 25 it searches all.
- [ ] **Step 3: Implement** the endpoints over `reachable_page`: `GET /api/v1/catalogs?after=&limit=` (default 100, max 1000), identity's `catalogs` and the labels endpoint's catalog list (same parameters, `next` beside the list), MCP `list_catalogs` (an optional `after` argument and a `next` field in its result). `list_changesets` without `catalog` filters changesets by the principal in SQL (`changesets::open_for_catalogs(names)` over the reachable names; for `Reach::All`, no catalog filter) instead of scanning the map. Config: `search_fanout_max` (default 25).
- [ ] **Step 4: The console** follows `next` where it lists catalogs (a "Load more" control, vanilla JS, as the rest of the console).
- [ ] **Step 5: Run, gates, commit** — `feat: paginated catalog listings and a search fan-out cap (M18 task 10)`.

---

### Task 11: Credential and user caches replace `KeyCache` (spec Decision 9)

The largest task. Keep `Principal`'s public shape; change where its parts come from.

**Files:**
- Create: `src/cache/credentials.rs`.
- Modify: `src/auth.rs`, `src/app.rs`, `src/http/console_session.rs`, `src/oauth/access.rs`, `src/oauth/token.rs`, `src/mcp/context.rs` (`admission_to`), `src/http/api/catalogs.rs` (`create_catalog`'s suspension read), `src/http/api/unlocks.rs` (`gates.admins`), `src/http/archives.rs` (`gates.catalogs`), `src/store/keys.rs` (`key_rows::by_hash`, `by_id`), `src/store/ceiling.rs` (`ceiling_of` by id), `src/store/users.rs`, `src/store/gates.rs`, `src/invalidate.rs`, `src/config.rs`, `tests/multi_replica.rs`.

- [ ] **Step 1: Store reads by one id** (each a single query, with tests):
  - `keys::record_by_hash(pool, &[u8; 32]) -> Option<KeyRecord>` and `record_by_id(pool, &str)`, reusing `key_rows::list`'s SELECT with a `WHERE` (`src/store/keys.rs:911`).
  - `ceiling::ceiling_by_user_id(pool, &str) -> Ceiling` (`CEILING_ROWS` filtered on the user).
  - `gates::for_user(pool, user_id, now) -> UserGates { member_of: HashSet<String>, entitled: HashSet<String>, is_admin: bool, unlocks: Vec<LiveUnlock>, audit_person: Option<i64> }` and `gates::audit_credential_id(pool, key_id) -> Option<i64>`, from the same queries `gates::load_with` runs, filtered.
  - `teams::ids_of_user(pool, user_id) -> Vec<String>` (for `team` invalidations; `teams.id` is text).

- [ ] **Step 2: Failing tests** (`src/cache/credentials.rs`):
  - `a_key_is_found_with_one_query_and_then_from_cache`.
  - `a_revoked_key_is_refused_after_its_credential_invalidation`.
  - `a_self_issued_key_is_capped_by_its_users_current_ceiling`: change the user's team membership; after the `Team` invalidation the key's reach shrinks.
  - `a_suspension_closes_the_catalog_for_every_cached_principal`: suspend the owner team; after `CatalogMeta`, `principal.require(Read, catalog)` is `Suspended` (read through the directory).
  - `an_unknown_credential_is_not_cached` (two lookups, two queries).
  - `entries_expire_after_credential_ttl` (`tokio::time::pause`).
  - Every existing test in `src/auth.rs` keeps passing.
  - `tests/multi_replica.rs`: the M16 revocation tests (`a_key_revoked_on_one_replica_is_refused_on_the_other_at_once`, the user delete, team suspension and grant revocation ones) pass unchanged with `poll_interval` and `credential_ttl` at 1 h.

- [ ] **Step 3: Implement.**

```rust
/// What a credential row says, before the user's ceiling caps it.
struct CredentialEntry { record: KeyRecord, expires_at: Option<DateTime<Utc>>, audit_credential: Option<i64> }

/// What a user row and its memberships say.
struct UserEntry { auth: UserAuth, ceiling: Ceiling, gates: UserGates, teams: Vec<String> }

/// Credentials and users on demand (spec Decision 9).
pub struct Credentials {
    by_hash: Bounded<[u8; 32], Arc<CredentialEntry>>,
    by_id: Bounded<String, Arc<CredentialEntry>>,
    users: Bounded<String, Arc<UserEntry>>,
    pool: PgPool,
    clock: Arc<dyn Clock>,
}

impl Credentials {
    pub async fn find(&self, presented: &str) -> Result<Option<Arc<Principal>>, OkfError>;
    pub async fn find_grant(&self, grant_id: &str) -> Result<Option<Arc<Principal>>, OkfError>;
    pub async fn user(&self, user_id: &str) -> Result<Option<Arc<UserEntry>>, OkfError>;
    pub async fn session_principal(&self, user_id: &str) -> Result<Option<Principal>, OkfError>;
    pub async fn apply(&self, invalidation: &Invalidation);
    pub fn mark_all_stale(&self);
}
```

- `find` and `find_grant` build the principal with today's `principal_of` logic (cap a self-issued reach by `UserEntry::ceiling`), and **not** cached as a whole: entries are cached, the cheap assembly runs per request, so a user change needs only the user entry dropped.
- `Principal`'s `suspended: Arc<Suspensions>` and `gates: Arc<AccessGates>` are replaced by `access: Arc<AccessFacts>`, where `AccessFacts` holds the user's `UserGates`, the credential's audit id, the `require_unlock` setting at assembly time, and a handle to the directory. `suspension(catalog)`, `check_not_suspended`, `admission`, `scope_of` and `access_record` read the catalog's facts through the directory row of the catalog being checked. Because the directory is async and these methods are sync today, `ToolContext` and the HTTP extractors resolve the catalog's row first (they already do so for the snapshot after Task 8) and pass `&CatalogRow` in: `principal.check_open(&row, access, now)`, `principal.require(access, &row)`. Change each call site listed in Verified facts accordingly; the compiler finds them.
- `apply`: `Credential { key_id }` invalidates `by_id[key_id]` and every `by_hash` entry whose `record.id == key_id` (`invalidate_where`); `User { id }` invalidates `users[id]`; `Team { id }` invalidates every user whose `teams` contains `id`; `CatalogMeta` and `Settings` touch nothing here (facts are read per request from the directory and settings).
- `Keys` and `KeyCache` are deleted, along with `App::keys()`, `reload_keys_now`, `reload_moved_keys`'s key reload and `catch_up_keys`. `BearerVerifier for Keys` becomes `for Credentials`. Settings stay on the `admin_generation` reload until Task 13. Labels stay on a `Labels::load` held by `App` and reloaded on any `User`, `Team` or `CatalogMeta` until Task 12 (a temporary, explicitly bounded regression: note it in the PR).
- `session_user` and `session_principal` (`src/http/console_session.rs:333`, `:376`) lose their cache-then-database fallback: the cache loads on miss.
- `src/oauth/access.rs` `find_grant` (`:327`) calls `credentials.find_grant` once; the catch-up retry goes.
- Config: `credential_ttl` (10 s), `credential_cache_entries`, `user_cache_entries` (100,000 each).

- [ ] **Step 4: Run** `gate.sh narrow auth`, `gate.sh narrow console_session`, `gate.sh narrow oauth`, `gate.sh narrow --test multi_replica`; full gates; commit — `feat: credentials and users load on demand (M18 task 11)`.

---

### Task 12: Labels on demand

**Files:** Create `src/cache/labels.rs`. Modify `src/labels/directory.rs`, every `keys().labels()` caller (Verified facts list them: `src/http/api/audit.rs:142, 170`, `src/http/archives.rs:407`, `src/http/api/catalogs.rs:247, 322, 355`, `src/http/api/labels.rs:128`, `src/http/api/identity.rs:185`, `src/http/api/people.rs:943, 979, 995, 1046, 1070, 1104, 1181`, `src/oauth/authorize.rs:756, 1075, 1242, 1265`, `src/mcp/context.rs:210`), `src/invalidate.rs`, `src/app.rs`.

- [ ] **Step 1: Failing tests:** `labels_for_returns_only_the_slugs_asked_for` (one query for users, teams and catalogs together); `a_user_invalidation_drops_that_users_label`; `an_audit_page_of_50_entries_reads_labels_once`.
- [ ] **Step 2: Implement.** `Labels` keeps its accessor API (`user`, `team`, `catalog`, `catalog_display_name`, `actor`, `catalog_owner`, `teams_of`, `catalog_choices`) but becomes a view over the slugs one response needs:

```rust
/// The slugs a response will show.
#[derive(Debug, Default)]
pub struct Wanted { pub users: BTreeSet<String>, pub teams: BTreeSet<String>, pub catalogs: BTreeSet<String> }

impl LabelLookup {
    /// Labels for `wanted`: cached ones, the rest in one round trip.
    pub async fn labels_for(&self, wanted: &Wanted) -> Result<Labels, OkfError>;
    pub async fn apply(&self, invalidation: &Invalidation);
}
```

Each caller builds its `Wanted` from what it is about to render (the audit page's actors and catalogs, the team view's members, ...) and calls `labels_for` once. `Labels::users()`/`teams()`/`catalog_slugs()` iteration callers (labels endpoint, `catalog_choices`) iterate the `Wanted` they built from the principal's reachable page (Task 10). `Labels::load` (all rows) stays only for the CLI (`src/cli/audit.rs:38, 46`), which runs once per command.

- [ ] **Step 3: Run, gates, commit** — `feat: labels on demand (M18 task 12)`.

---

### Task 13: Settings on invalidation, and the M16 bridge (spec Decisions 9, 14)

**Files:** Modify `src/app.rs`, `src/invalidate.rs`, `src/shared/announce.rs`, `src/http/api/mod.rs`, `tests/multi_replica.rs`.

- [ ] **Step 1: Failing tests:**
  - `a_settings_invalidation_reloads_the_settings` (lib).
  - `an_m16_announcement_marks_credentials_and_settings_stale` (lib, through `announce::run` with the test `Source`): the next `find` re-reads the key.
  - `an_administrative_http_write_publishes_the_m16_announcement` (`tests/multi_replica.rs`, Redis on): subscribe to the M16 channel and assert a message after `POST /api/v1/keys/{id}/revoke`.
- [ ] **Step 2: Implement.**
  - `App::apply(Settings)` reloads `settings::load`. `reload_moved_keys` and the `admin_generation`/`announced_generation` atomics go; `settle` (`src/http/api/mod.rs:306`) becomes `bridge(app)`: when `[redis]` is set, `announce::publish(redis, None)` so M16 replicas reload; otherwise nothing. Every `settle` call site keeps calling it, renamed. `src/oauth/authorize.rs:1141`'s `app.poll()` becomes `bridge(app)` too.
  - `announce::listen` stays subscribed; any message calls `app.apply_m16_announcement()`: `credentials.mark_all_stale()`, labels and settings reload. It no longer calls `reload_keys_now` (gone in Task 11).
  - Each bridge item carries `// M16 bridge (spec Decision 14): remove in the release after 0.23.` so Task 18's follow-up issue can grep them.
- [ ] **Step 3: Run, gates, commit** — `feat: settings on invalidation; bridge to M16 replicas (M18 task 13)`.

---

### Task 14: Every replica runs a worker (#458, spec Decision 17)

**Files:** Modify `src/mail/queue.rs`, `src/config.rs`, `src/http/mod.rs`, `tests/support/server.rs`, `src/mcp/testing.rs`.

- [ ] **Step 1: Failing tests** (`src/mail/queue.rs`):
  - `a_dead_workers_job_returns_within_a_minute`: start a worker with production timing except `heartbeat = 1s` (keeping `missed_heartbeats = 6`), take a job, kill the worker task without shutdown, assert another worker sends it within 6-8 s (scaled from 60 s).
  - `stale_worker_rows_are_pruned_and_live_ones_kept`: insert an `apalis.workers` row with `last_seen` an hour ago and one referenced by a `Running` job; `prune_workers` deletes only the first.
  - `mail_worker_false_parses_with_a_warning_and_is_ignored` (`src/config.rs`).
  - The existing `shutdown_finishes_the_mail_in_hand` and `two_workers_never_send_the_same_mail_twice` pass unchanged.
- [ ] **Step 2: Implement.**
  - `HEARTBEAT_INTERVAL = 10s`, `QueueConfig::...heartbeat_interval(10s).missed_heartbeats(6)`.
  - One worker pool per process, `WORKER_POOL_SIZE = 3` (mail's LISTEN, mail's fetch/ack/heartbeat and handler writes, housekeeping's fetch/ack/heartbeat; housekeeping polls without LISTEN), exposed as `queue::WorkerPool` and passed to Task 15's worker. Update the module docs' connection arithmetic.
  - `pub async fn prune_workers(pool: &PgPool, now: DateTime<Utc>) -> Result<u64, OkfError>`: `DELETE FROM apalis.workers w WHERE w.last_seen < $1 AND NOT EXISTS (SELECT 1 FROM apalis.jobs j WHERE j.lock_by = w.id AND j.status IN ('Queued','Running'))` with `$1 = now - 10 x orphan timeout`. Called from the poll's hourly prune for now; Task 15 moves it.
  - `mail.worker`: still deserializes; `Config::validate` logs `warn!("mail.worker is deprecated and ignored: every serve runs the worker")`. `serve` always spawns the worker; tests stop it through a new `App`/`serve` constructor flag (`ServeOptions { worker: bool }`), which `tests/support/server.rs` (`serve_app`) and `src/mcp/testing.rs` set.
  - Shutdown: `worker_shutdown_grace` (default 30 s): `serve` waits for the worker's `run_until` at most that long, then logs and returns.
- [ ] **Step 3: Run, gates, commit** — `feat: an HA-tolerant worker on every replica (M18 task 14, closes #458)`.

---

### Task 15: Housekeeping as apalis jobs (#356, spec Decisions 15, 16)

**Files:**
- Create: `src/housekeeping.rs`, `migrations/0023_cluster_stats.sql`.
- Modify: `src/app.rs` (prune functions move), `src/http/mod.rs`, `src/telemetry/metrics.rs`, `src/mail/queue.rs` (`record_depth` returns counts instead of recording), `tests/multi_replica.rs`.

- [ ] **Step 1: Migration.**

```sql
-- One row: the cluster-wide counts the cluster_stats job computes, read by
-- every replica's gauges (spec Decision 16).
CREATE TABLE cluster_stats (
    id              boolean PRIMARY KEY DEFAULT true CHECK (id),
    open_changesets bigint NOT NULL,
    mail_queue      jsonb  NOT NULL,
    computed_at     timestamptz NOT NULL
);
```

- [ ] **Step 2: Failing tests** (`src/housekeeping.rs`):

```rust
#[tokio::test]
async fn two_replicas_scheduling_one_slot_create_one_job() {
    let db = TestDb::start_migrated().await;
    let slot = Task::PruneOauth.slot_at(fixed_now());
    schedule(&db.pool, Task::PruneOauth, slot).await.unwrap();
    schedule(&db.pool, Task::PruneOauth, slot).await.unwrap(); // the duplicate is ignored, not an error
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM apalis.jobs WHERE job_type = $1")
        .bind(QUEUE).fetch_one(&db.pool).await.unwrap();
    assert_eq!(jobs, 1);
}

#[test]
fn slots_start_at_their_cadence_boundary() {
    // 2026-11-18T19:52:10Z -> hourly slot 19:00:00, 5-minute slot 19:50:00, daily slot 00:00:00.
}
```

Plus one test per task that runs `run(task, &context)` against fixtures and asserts its rows are gone (moving today's `App` prune tests here), and `cluster_stats_are_reported_by_every_replica` (two `App`s, one row, both gauges equal).

- [ ] **Step 3: Implement.**

```rust
/// The apalis queue (`apalis.jobs.job_type`).
pub const QUEUE: &str = "housekeeping";

/// One housekeeping task (spec Decision 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Task { PruneChangesets, PruneOauth, PruneAudit, PruneWorkers, ClusterStats }

impl Task {
    pub const ALL: [Task; 5] = [ /* ... */ ];
    pub fn cadence(self) -> Duration; // 5 min, 1 h, 24 h, 1 h, 60 s
    /// The start of the cadence window containing `now`, in Unix seconds.
    pub fn slot_at(self, now: DateTime<Utc>) -> i64;
}

/// The job: which task, for which slot.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Housekeeping { pub task: Task, pub slot: i64 }

/// Pushes `task`'s job for `slot` unless one exists: the idempotency key
/// `"<task>:<slot>"` and apalis's unique index make a second push a unique
/// violation, which is ignored. On the pool, in its own statement, so the
/// violation aborts nothing else.
pub async fn schedule(pool: &PgPool, task: Task, slot: i64) -> Result<(), OkfError>;

/// Every minute until `shutdown`: schedules the current slot of every task.
pub async fn tick_until(app: Arc<App>, shutdown: CancellationToken);

/// The worker (Task 14's pool), and the handler that runs one job.
pub fn spawn_worker(app: Arc<App>, pool: WorkerPool, shutdown: CancellationToken) -> JoinHandle<()>;
async fn run(job: Housekeeping, context: Data<HousekeepingContext>) -> Result<(), BoxDynError>;
```

- `schedule` recognizes the unique violation as `sqlx::Error::Database(e) if e.code().as_deref() == Some("23505")` and returns `Ok(())`.
- The tasks are today's functions moved from `App`: `prune_empty_changesets`, `prune_oauth` (minus its in-process once-an-hour gate, now the cadence; it keeps calling `revoke_idle_grants` before the token prune), `prune_audit` (minus its gate), `queue::prune_workers` (Task 14), and `cluster_stats`: `changesets::open_counts` summed, `queue::record_depth`'s grouped count, written to the row with `INSERT ... ON CONFLICT (id) DO UPDATE`. `prune_oauth` also deletes `Done` housekeeping jobs older than a day.
- `max_attempts(3)`; a failure returns a plain error so apalis retries.
- Each replica's gauges (`okf.changesets.open` without the `catalog` label, `okf_mail_queue_depth`) read an in-memory copy of the row that `tick_until` refreshes each minute.
- Metrics: `okf.housekeeping.runs{task,outcome}`, `okf.housekeeping.duration{task}`.
- `serve` spawns `tick_until` and the housekeeping worker and awaits both at shutdown. `poll_pass` drops the prune and count calls.

- [ ] **Step 4: Run, gates, commit** — `feat: housekeeping runs once per cluster as apalis jobs (M18 task 15, closes #356)`.

---

### Task 16: Remove the poll loop and per-catalog series (spec Decisions 18, 19)

**Files:** Modify `src/app.rs`, `src/http/mod.rs`, `src/config.rs`, `src/telemetry/metrics.rs`, `src/cli.rs`, `tests/support/server.rs`, any test that calls `app.poll()`.

- [ ] **Step 1:** Delete `App::poll`, `poll_pass`, `poll_until`, the `polling` mutex, the `okf.poll` span and `poll_duration` metric, `catalog_refreshes` / `okf_catalog_refreshes_total` and `RefreshResult`, and the `okf.catalog.concepts` and `okf.catalog.generation` gauges (`src/telemetry/metrics.rs:493`, `:510`). Remove them from `Metrics::FAMILIES`.
- [ ] **Step 2:** `poll_interval` still parses and logs `warn!("poll_interval is deprecated and ignored")`. `/healthz` drops `catalogs` (`src/http/mod.rs:229`); add `"cached_catalogs"` from the cache's entry count if the healthz tests want a number (they assert the shape: update them).
- [ ] **Step 3:** Every test that called `app.poll().await` to make a change visible now relies on `NOTIFY`: replace with a bounded wait helper `app.until_applied(|app| ..)` (lib) or `eventually` (integration). `grep -rn "\.poll()" src tests` must find nothing.
- [ ] **Step 4:** Full gates; commit — `feat: remove the poll loop (M18 task 16)`.

---

### Task 17: Docs

**Files:** `docs/operations.md`, `docs/reference/` (the API and tool references for the paginated listings, the search cap, the new settings and the removed ones), `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` (the "In-memory snapshot" section points to the M18 spec), `changelog/`.

- [ ] **Step 1:** In `docs/operations.md`:
  - a "Caches" section: each cache, its setting, what bounds it, its metrics, and how to size `catalog_cache_bytes` from `okf.cache.bytes` and hit ratios;
  - "Change propagation": the change feed, the 10 s TTL bound, what a listener reconnect does;
  - replace "Pruning" with "Housekeeping": the five tasks, cadences, one run per slot, `okf.housekeeping.runs`;
  - "Postgres connections": per replica `database_max_connections` + 1 (listener) + 3 (worker pool); total for N replicas;
  - the mail worker: every replica, at-least-once delivery, orphan recovery about 1 minute, `worker_shutdown_grace` matched to `terminationGracePeriodSeconds`;
  - optional ingress hashing on `Mcp-Session-Id` (nginx `hash $http_mcp_session_id consistent;`), what it buys, and that nothing depends on it;
  - Grafana notes: database-wide gauges with `max()`, the removed per-catalog series and their replacements.
- [ ] **Step 2:** A release-notes draft in the changelog entry: removed metrics, paginated responses (`next`), the search fan-out cap, deprecated settings (`poll_interval`, `overlay_cache_size`, `mail.worker`), the new settings with defaults.
- [ ] **Step 3:** Commit — `docs: bounded memory operations and reference (M18 task 17)`.

---

### Task 18: Scale test, smoke test, milestone changelog

**Files:** Create `tests/scale.rs`. Modify `README.md`, `changelog/<ts>-m18-milestone.md`.

- [ ] **Step 1: `tests/scale.rs`** (`#[ignore]`, run in the close-out with `gate.sh narrow --test scale -- --ignored`):
  - seed 1,000,000 catalogs (one tiny document each) and 1,000,000 users with one key each, by `COPY`/`generate_series` SQL, not the store functions;
  - assert `App::load` completes in under 5 s and holds no catalog;
  - with `catalog_cache_bytes` set to fit 1,000 tiny catalogs, read 5,000 distinct catalogs; assert `okf.cache.bytes{cache=catalog}` never exceeds the budget and the process's RSS growth (from `/proc/self/statm`) stays under budget plus 200 MiB;
  - authenticate 10,000 distinct keys; p99 under 20 ms on the test machine; `credential` cache entries bounded by `credential_cache_entries`;
  - `GET /api/v1/catalogs` for an admin key: first page under 50 ms.
  Record the measured numbers in the milestone changelog.
- [ ] **Step 2: Smoke test** (by hand, documented in the changelog): two replicas against one Postgres, publish on one, read on the other; revoke via the CLI, refused on both; kill one replica's listener connection and repeat.
- [ ] **Step 3:** README feature list; the milestone changelog; a follow-up issue "Remove the M16 bridge and deprecated settings" listing every `M16 bridge` comment.
- [ ] **Step 4:** Commit — `docs: milestone 18 changelog and scale results (M18 task 18)`.

## Self-review

- **Spec coverage.**
  - Decision 1 (one milestone): Scope.
  - Decision 2 (catalog cache): Tasks 4, 8 (weight factor measured in 8).
  - Decision 3 (pinning): Task 8.
  - Decision 4 (locks): Task 6.
  - Decision 5 (overlays): Task 9.
  - Decision 6 (directory): Task 5.
  - Decision 7 (listing): Tasks 5 (queries), 10 (endpoints).
  - Decision 8 (fan-out cap): Task 10.
  - Decision 9 (credential caches): Tasks 11, 12, 13.
  - Decision 10 (change feed): Tasks 1, 2.
  - Decision 11 (epochs): Tasks 4, 5.
  - Decision 12 (reconnect): Tasks 1, 4 (`mark_all_stale`), each cache's `apply_reconnect`.
  - Decision 13 (TTLs): Tasks 5, 8, 11.
  - Decision 14 (mixed versions): Task 13.
  - Decision 15 (housekeeping, #356): Task 15.
  - Decision 16 (gauges): Task 15.
  - Decision 17 (#458): Task 14.
  - Decision 18 (poll removed): Task 16.
  - Decision 19 (metrics): Tasks 1, 4, 15, 16.
  - Decision 20 (audit rows): Task 3.
  - Testing section: each task's tests; the scale test in Task 18.
  - Docs section: Task 17.
- **Names used across tasks:**
  - `invalidate::{Invalidation, Message, CHANNEL, invalidate, listen}`, `App::{apply, apply_reconnect}`;
  - `cache::{Bounded, CacheName, Limit}`, `Bounded::{get_or_load, get_or_revalidate, peek, invalidate, invalidate_where, mark_all_stale, with_weigher, with_idle}`;
  - `cache::directory::{CatalogDirectory, Page}`, `CatalogDirectory::{resolve, resolve_many, reachable_page, apply}`; `store::catalogs::{CatalogRow, row_by_name, row_by_id, rows_by_names, page}`;
  - `cache::catalogs::CatalogCache::{get, apply}`; `App::{catalog_row, catalog_state, publish_lock, directory}`;
  - `cache::credentials::{Credentials, CredentialEntry, UserEntry, AccessFacts}`; `cache::labels::{LabelLookup, Wanted}`;
  - `housekeeping::{QUEUE, Task, Housekeeping, schedule, tick_until, spawn_worker}`; `mail::queue::{WorkerPool, prune_workers}`;
  - `bridge(app)` (Task 13).
- **Known risk.** Task 11 changes the signatures of `Principal::require` and `check_open` to take a `&CatalogRow`, touching about 40 call sites across HTTP and MCP. It is the largest PR; if review shows it is too big, split it as 11a (credential and user caches, principals still carrying a suspension snapshot built per request from the directory) and 11b (the `&CatalogRow` signatures).

## Open questions for the maintainer

None blocking. To confirm when filing the epic: the defaults for `catalog_cache_bytes` (1 GiB) and `search_fanout_max` (25), which Task 18's scale test will inform.

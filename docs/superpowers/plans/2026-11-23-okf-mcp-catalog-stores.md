# OKF MCP Server, catalog stores: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

> **Builds on `main` at `22c07f7`** (PR #951).

**Goal:** Catalog data (documents, history, changesets, maintenance, catalog audit) can live in a database apart from the account database. A new catalog lands on its team's pinned store or the current default store, and every catalog read and write is routed to its store. Production keeps one database.

**Architecture:**
- **`src/stores/`** (new) owns the two database handles, `AccountDb` and `CatalogDb`, the `Stores` registry (rows of `stores`, pools opened on first use, credentials fetched per kind) and placement.
- **`App`** holds `accounts: AccountDb` and `stores: Stores` instead of `pool: PgPool`. `App::pool()` goes away at the end of release 1.
- **Store-layer functions** take `&AccountDb` or `&CatalogDb` at their entry points (or a transaction begun on one). Internal helpers keep `&mut PgConnection`.
- **Schemas:** account tables stay in `public`; catalog tables move to `catalog`. Every account pool runs with `search_path = public`, every store pool with `search_path = catalog`, so a query sent to the wrong database fails in tests even when both live in one database.
- **Two migration folders:** `migrations/accounts/` (today's 0001-0035 and the new account migrations) and `migrations/catalog/` (a baseline for new stores, then catalog migrations).

**Tech Stack:** Rust 2021 with the existing axum 0.8 / sqlx 0.9 / rmcp / tokio / apalis 1.1 stack. New dependencies (Task 9): `aws-sdk-secretsmanager`, and RDS IAM token signing (`aws-sigv4` directly, with `aws-credential-types`, `aws-smithy-runtime-api` and `http`; decided in Task 9, see its note), both on the `aws-config` already in the tree.

**Spec:** `docs/superpowers/specs/2026-11-23-okf-catalog-stores-design.md` (Decisions 1-18; Decisions 5, 7 and 13 were refined and 16-18 added during planning, below).

---

## Conventions for every task

- **Branches and PRs.** Branch per task from the milestone branch, one PR per task, linked to its sub-issue. Follow `AGENTS.md` (review rounds, keeping branches current, `tools/agentic/`) and `tools/agentic/brief-common.md`.
- **TDD.** Write the failing test, run it, implement, run it, commit.
- **Gates** run only through `tools/agentic/gate.sh`: `fmt`, `lint`, `test` and `dylint` before every push. While iterating, use `gate.sh narrow <cargo test args>`.
- **No full implementation code in this plan.** Code here is limited to what several tasks share: migration SQL, the signatures of shared types, config keys and dependencies. Each task is described in prose (behaviour, edge cases, files, named tests); the implementer writes the code.
- **Code style.** Comments explain Rust-specific constructs for a Python-expert, Rust-novice reader, as in the existing code.
  - Every public item gets a doc comment with `# Arguments`, `# Returns` and `# Errors`.
  - No `.unwrap()` outside tests.
  - Errors are existing `OkfError` variants where one fits; a new `OkfError::StoreUnavailable { store }` (503) is added in Task 1.
  - Every new function has `#[tracing::instrument(.., skip_all)]` or a reason-bearing `uninstrumented_fn` allow.
- **Privacy rules** (review checklist items for every task):
  - the `stores` table never holds a credential, and no code path logs one (a connection URL is logged with its password removed, or not at all);
  - job payloads carry ids only, never catalog content;
  - a store's name may appear in logs and metrics labels; a catalog name may not appear in metric labels.
- **Behaviour is preserved unless the spec says otherwise.** Existing tests keep passing, apart from constructor arguments and handle types.
- **Changelog.** One entry per task, `changelog/<UTC ts>-stores-task-<NN>-<slug>.md`, with `## Added`, `## Changed` and `## Notes`. Take the timestamp from `date -u +%Y-%m-%dT%H-%M-%SZ`. Task 20 also writes the milestone changelog.
- **Placeholders only** in tests and docs: `example.com`, `db.example.internal`, `alice`, store names like `shared-1` and `acme`. No real hostnames, account ids, ARNs or names.
- **If an implementation has to differ from this plan,** fix the plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.
- **Commit messages** end with the attribution line the coordinator gives.

## Verified facts the plan relies on (at `22c07f7`)

- `App` holds one `pool: PgPool` (`src/app.rs:70`) exposed as `App::pool()` (`src/app.rs:535`). About 195 production uses of `.pool()` and about 340 production signatures taking `PgPool`, `PgConnection` or a Postgres transaction, outside `#[cfg(test)]` modules. Largest users: `src/http/signin.rs`, `src/housekeeping.rs`, `src/http/api/catalogs.rs`, `src/http/api/keys.rs`, `src/store/audit.rs`, `src/store/users.rs`, `src/store/teams.rs`, `src/mail/queue.rs`.
- Migrations run through one `sqlx::migrate!("./migrations")` (`src/db.rs:22`), applied by `db::migrate` (`src/db.rs:110`) under advisory lock `CONCURRENT_INDEX_LOCK`, followed by apalis's own migrations in schema `apalis` with history `apalis._sqlx_migrations` (`src/db.rs:96`). sqlx 0.9's `Migrator::dangerous_set_table_name` can place a migrator's history table elsewhere.
- **Statements that join catalog tables with account tables** (scan of production SQL literals):
  - `src/store/changesets.rs:654`: lists changesets with their catalog's name and key (`JOIN catalogs`, `LEFT JOIN api_keys`);
  - `src/store/repair.rs:281`: `documents` joined with `catalogs`;
  - `src/store/users.rs:1268`: a user's catalogs with open changesets (`changesets`, `api_keys`, `users`, `catalogs`);
  - `src/store/users.rs:1494`: `discard_open_changesets` for a deleted user (`changesets`, `changeset_entries`, `api_keys`, `catalogs`);
  - `src/store/maintenance/runs.rs:481`: `INSERT INTO maintenance_runs` with `(SELECT id FROM api_keys WHERE id=$3)`.

  Statements built with `format!` or split over several literals are not covered by the scan. Task 2 re-checks with the search_path test harness.
- **`catalogs.generation`** is read and bumped by `src/store/publish.rs:282` and `:392`, `src/store/repair.rs:150` and `src/catalog.rs:26`.
- **Catalog row locks coordinate writes with deletion:**
  - publish and import take `pg_advisory_xact_lock(CATALOG_LOCK_NAMESPACE, id)` (`catalogs::lock`, `src/store/catalogs.rs:1380`);
  - changeset writes take `FOR KEY SHARE` on the `catalogs` row (`changesets::lock_catalog`, `src/store/changesets.rs:239`);
  - `catalogs::delete` takes `FOR UPDATE` on it (`src/store/catalogs.rs:1242`) and runs `audit::lock_people` in the same transaction.
- **Audit:**
  - entries are written by `audit::record(conn, actor, entry, now)` (`src/store/audit.rs:413`) inside the caller's transaction;
  - catalog-store actions are recorded in `src/store/publish.rs:289` (published), `src/store/import.rs:301` (imported), `src/store/repair.rs:159` (repaired) and `src/write.rs:130` (`apply_audited`, reverted);
  - `audit_person()` (`migrations/0012_audit.sql`) reads `users` `FOR KEY SHARE`.
- **Database triggers that the split touches:**
  - `audit_entries_append_only` and `audit_entries_no_truncate` (`migrations/0012_audit.sql:163-184`) refuse any UPDATE, and any DELETE of an entry inside the retention period. The period is read from `server_settings`.
  - `maintenance_runs_audit_end` and `maintenance_runs_audit_empty` (`migrations/0017_maintenance_audit.sql:27-63`) run `audit_maintenance_run()`. It reads `audit_credentials`, `api_keys`, `users`, `audit_teams` and `catalogs`, and writes the account `audit_entries`. `SET SCHEMA` keeps triggers attached, so this one would break as soon as the store pool's `search_path` is `catalog`.
- **Unlock tallies** run as their own statement on the pool (`src/app.rs:424`, `src/store/unlocks.rs:845`), not in a catalog transaction.
- **Cache signalling:** `src/invalidate.rs` listens on one channel on the one database. `CatalogDirectory` (`src/cache/directory.rs`) caches catalog rows by name; `App::catalog_row` (`src/app.rs:1067`) and `App::catalog_state` (`src/app.rs:1087`) are the entry points routes and tools use.
- **Housekeeping tasks** (`src/housekeeping.rs:117`): `PruneChangesets`, `PruneOauth`, `PruneAudit`, `PruneWorkers`, `ClusterStats`, each scheduled once per slot with idempotency key `"<task>:<slot>"`.
- **Test databases** come from `tests/support/pg.rs`: a migrated template database copied per test (`TestDb::start_migrated`, `ensure_template`, `create_database`). `db::test_db()` (`src/db.rs:191`) adds a fixture user.

## Decisions taken while planning (2026-11-23)

The spec already carries these: P1 is its Decision 5, P2 Decision 7, P3 Decision 13, P4-P6 Decisions 16-18.

- **P1 (refines Decision 5): pools are never shared.** Every pool sets its `search_path` at connect (`public` for the account pool, `catalog` for a store pool), so an unqualified catalog query sent to the account pool, or an unqualified account query sent to a store, fails with "relation does not exist" even when the store lives in the account database. Two rules keep that true (review of PR #956):
  - **No table name exists in both schemas.** The store's audit table is `catalog_audit_entries`, not a second `audit_entries`, so an unqualified audit DELETE on the wrong pool fails instead of pruning the wrong log. A test lists table names in `public` and `catalog` and asserts they don't overlap: `no_table_name_is_in_both_schemas`.
  - **Rust SQL never names a schema.** Only migrations write `catalog.` or `public.`. A test scans `src/` for schema-qualified table names in SQL literals and fails on one: `production_sql_never_qualifies_a_schema`.

  This is a test-time guard against mistakes, not a security boundary: one database user can still reach both schemas when they share a database. Separate privileges per side are out of scope. A single-database install therefore opens two pools to one database. The store pool's size is the new config key `store_max_connections` (default 8), beside the existing `database_max_connections` (account pool).
- **P2 (refines Decision 7): types at the entry points, search_path behind them.** `AccountDb` and `CatalogDb` make call sites in routes, tools, the CLI and jobs pick the right database at compile time. Store-layer helpers that take `&mut PgConnection` are not type-checked against the wrong side; the search_path isolation of P1 catches those at test time, from Task 11 on.
- **P3 (refines Decision 13): first-match reads fail rather than skip.** A tool that answers from the first selected catalog holding a path (`get_concept`, `history`, `read_document` without `catalog`) returns `StoreUnavailable` when a catalog earlier in the order is on an unreachable store, instead of answering from a later one; the earlier catalog might have held the answer. Merging reads (`search`, catalog listings) return their results plus `unavailable`.
- **P4: which audit actions live on a store.** Actions recorded inside a catalog-store transaction go to `catalog.catalog_audit_entries`: `catalog.published`, `catalog.imported`, `catalog.repaired`, `catalog.reverted`, `catalog.changeset_discarded`, `catalog.maintenance_run`. All others, including `catalog.created`, `catalog.deleted`, `catalog.exported`, `catalog.admin_read` and the unlock actions, stay in the account database. Task 12 confirms the list against the code and corrects it here if a write site differs.
- **P5: catalog audit outlives the catalog.** Deleting a catalog's store data (Task 13) leaves its `catalog.catalog_audit_entries` rows; they are pruned by `audit_retention_days`, per store, like account audit.
- **P6: a deleted catalog's name stays reserved until its store data is gone.** `catalogs.deleted_at` marks the registry row; the cleanup job removes the row after the store side is deleted, and only then can the name be reused.

- **P7 (from review of PR #956): catalog-event entries written before the split stay in the account audit table.** The append-only trigger forbids deleting recent entries, and suspending it would weaken the guarantee it exists for. The merged view reads both tables, so each entry still appears exactly once. Only entries written after the split go to `catalog.catalog_audit_entries`.
- **P8 (from review of PR #956): separate stores stay disabled until the whole epic has landed.** Until Task 20, adding a store with location `host` is refused, so `main` (inside the account database) is the only store that can hold catalogs. Each release can therefore be deployed without a half-built path to a separate database. Task 20 lifts the restriction once the end-to-end test passes.
- **P9 (from review of PR #956): create and recovery coordinate through the catalog's lock on the store.** Both take `pg_advisory_xact_lock(CATALOG_LOCK_NAMESPACE, id)` on the store, the publish lock that already exists.
  - Create's store transaction, holding it, first checks on the account database that the registry row still exists and is unprovisioned. It aborts if not.
  - Recovery, holding it, checks the store side. If present, it sets `provisioned_at`. If absent, it deletes the registry row and commits that before releasing the lock.
  - So a stalled create either finishes first, and recovery sees its data, or waits behind recovery, then finds its row gone and writes nothing. There's no time threshold to get wrong.
- **P10 (from review of PR #956): release 3 deploys in a maintenance window, not as a rolling upgrade.** Release-2 replicas query tables in `public` and read `catalogs.generation`, and migrations 0041-0042 remove both. Procedure: snapshot the database; scale `serve` to zero; run `okf-mcp migrate` (account migrations, then adoption); start the new release. Task 19 writes it into `docs/operations.md`, and Task 20's smoke test follows it.

- **P11 (from review of PR #956): release 2 is compatible with release-1 replicas.** Migration 0036 gives `catalogs.store` the default `main` and `provisioned_at` the default `now()`, so a release-1 create still works and lands provisioned. Hiding unprovisioned rows ships in Task 8, and two-step creation (Task 10b) ships later as release 2b, so no replica that would show a half-created catalog is still running when the first one is written.
- **P12 (from review of PR #956): deleting a catalog holds the store's row lock across the account commit.** A store transaction holding `catalog_state` `FOR UPDATE` wraps the account transaction, and `catalog_state.deleted_at` tells writers that waited behind it that the catalog is gone. Without force, a delete never discards work written during it (Task 13).
- **P13 (from review of PR #956): store cleanup is owed in the transaction that causes it.** `store_cleanups`, in the account database, gets its rows in the deleting transaction itself, keeping the ids the cleanup needs. A crash after commit can't lose a cleanup (Task 13).

## Scope

In: Decisions 1-18 of the spec, P1-P13, releases 1-3 of the spec's migration, the tests of its Testing section, and its Docs section.

Out (spec non-goals): moving a catalog between stores; revision storage changes (#954); dedicated cells; per-store connection budgets; store states.

## File map

| Path | Change | Responsibility |
|---|---|---|
| `src/stores/mod.rs` | new | `AccountDb`, `CatalogDb`, `StoreName`; re-exports |
| `src/stores/registry.rs` | new | `Stores`: rows of `stores`, pools on first use, idle shrink |
| `src/stores/credentials.rs` | new | credential kinds `env`, `secrets-manager`, `iam` |
| `src/stores/placement.rs` | new | choosing a new catalog's store |
| `src/store/stores.rs` | new | SQL for `stores`, `default_store`, team pins |
| `src/app.rs` | modify | `accounts` and `stores` replace `pool` |
| `src/db.rs` | modify | two migrators, adoption, `migrate_store` |
| `migrations/` -> `migrations/accounts/` | move | today's migrations, unchanged |
| `migrations/accounts/0036_stores.sql` to `0040_catalogs_unprovisioned_index.sql` | new | Task 7 |
| `migrations/accounts/0041_catalog_schema.sql` | new | Task 11 |
| `migrations/catalog/0001_baseline.sql` | new | Task 14 |
| `src/store/*.rs`, `src/store/maintenance/*.rs` | modify | entry points typed by side |
| `src/catalog.rs`, `src/overlay.rs`, `src/write.rs` | modify | catalog side |
| `src/store/audit.rs` | modify | catalog audit writes; merged reads |
| `src/http/api/audit.rs`, `src/http/api/audit/cursor.rs` | modify | merged view and cursor |
| `src/invalidate.rs` | modify | one listener per opened store |
| `src/housekeeping.rs` | modify | per-store tasks |
| `src/mcp/context.rs`, `src/mcp/search.rs`, `src/mcp/history.rs`, `src/mcp/documents.rs` | modify | fan-out and `unavailable` |
| `src/http/api/stores.rs` | new | admin API: stores, default store, team pins |
| `src/cli/stores.rs` | new | CLI: same, plus `migrate-stores` |
| `tests/support/pg.rs` | modify | a second database per test; store mode |
| `tests/stores.rs` | new | end-to-end on two stores |
| `docs/operations.md`, `docs/reference/tools.md`, admin API reference | modify | Task 19 |

## Shared interfaces

Signatures only; bodies are the implementers'.

```rust
// src/stores/mod.rs

/// A store's name, as on its `stores` row: `^[a-z0-9-]+$`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct StoreName(String);

impl StoreName {
    pub fn parse(name: &str) -> Result<Self, OkfError>;
    pub fn as_str(&self) -> &str;
    /// `main`, the store every existing catalog is on after migration 0036.
    pub fn main() -> Self;
}

/// The account database. Cloning is cheap (the pool is reference-counted).
#[derive(Debug, Clone)]
pub struct AccountDb { /* PgPool */ }

impl AccountDb {
    pub fn new(pool: PgPool) -> Self;
    pub async fn begin(&self) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, OkfError>;
    /// For store-layer functions taking an executor; not for routes.
    pub fn executor(&self) -> &PgPool;
}

/// One store's database.
#[derive(Debug, Clone)]
pub struct CatalogDb { /* StoreName, PgPool */ }

impl CatalogDb {
    pub fn store(&self) -> &StoreName;
    pub async fn begin(&self) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, OkfError>;
    pub fn executor(&self) -> &PgPool;
}

// src/stores/registry.rs

/// Every store's pool, opened on first use.
pub struct Stores { /* .. */ }

impl Stores {
    /// The store's database, opening its pool (and fetching its credential)
    /// on first use.
    ///
    /// # Errors
    /// `OkfError::StoreUnavailable` if the pool cannot connect;
    /// `OkfError::NotFound` for a store with no row.
    pub async fn get(&self, store: &StoreName) -> Result<CatalogDb, OkfError>;
    /// Every store's row (for fan-out to all stores: housekeeping, audit).
    pub async fn all(&self) -> Result<Vec<StoreName>, OkfError>;
    /// Drops a store's pool, so the next `get` reconnects with a fresh row
    /// (called on a `stores` invalidation).
    pub async fn forget(&self, store: &StoreName);
}

// src/app.rs
impl App {
    pub fn accounts(&self) -> &AccountDb;
    pub fn stores(&self) -> &Stores;
    /// The catalog's store database (from `row.store`).
    pub async fn catalog_db(&self, row: &CatalogRow) -> Result<CatalogDb, OkfError>;
}
```

> **Corrected (issue #959, 2026-11-23).** Shipped in Task 1: `CatalogDb::new(store, pool)` was `pub(crate)` here, and is `pub` from issue #960 (see the Task 2 note), `StoreName` also implements `Display` and refuses names over 63 characters, `AccountDb::new` is `pub`, and `OkfError::store_unavailable(&StoreName)` is the constructor that logs the store name (local half only); build the error through it, not the bare variant. `App::main_store()` returns `&CatalogDb`, not an owned `CatalogDb`; clone it where an owned handle is needed.

Config keys (`src/config.rs`): `store_max_connections` (u32, default 8, must be > 0), `store_idle_timeout` (humantime, default `10m`), and `store_connect_timeout` (humantime, default `5s`, the time after which `Stores::get` returns `StoreUnavailable`).

## Task dependencies

```
Release 1:  1 -> 2 -> 3 -> 4 -> 5 -> 6
Release 2:  6 -> 7 -> 8 -> 9, 10 (parallel)
Release 2b: 10 -> 10b
Release 3:  10b -> 11 -> 12 -> 13 -> 14 -> 15 -> 17
Then:       17 -> 16, 18 (parallel) -> 19 -> 20 (lifts P8)
```

Each release ends deployable: after Task 6 (types only, rolling), after Task 10 (registry and placement, rolling), after Task 10b (two-step create, rolling, once release 2 is on every replica: P11), after Task 17 (the split, in a maintenance window: P10). Separate stores become usable only after Task 20 (P8).

---

## Release 1: types only

### Task 1: `AccountDb`, `CatalogDb`, `StoreName`

**Files:** create `src/stores/mod.rs`; modify `src/lib.rs`, `src/app.rs`, `src/error.rs`.

- [ ] Add the types of "Shared interfaces" (not `Stores` yet). For release 1, `App` gets `accounts()` and a temporary `App::main_store() -> &CatalogDb` that wraps the same pool with store name `main`. `App::pool()` stays and is marked `#[deprecated(note = "use accounts() or catalog_db()")]`, with the deprecation allowed crate-wide until Task 6.
- [ ] Add `OkfError::StoreUnavailable { store: String }`, mapped to 503 in HTTP and to a tool error saying the catalog's data is temporarily unavailable. The message names the store only in the local log half (`split_event!`), never to the caller.
- [ ] Tests: `store_name_parse_accepts_slugs_and_refuses_the_rest`; `main_store_and_accounts_reach_the_same_database_in_release_one`; `store_unavailable_is_503_and_hides_the_store_name`.

### Task 2: catalog writes take `CatalogDb`

**Files:** `src/write.rs`, `src/store/publish.rs`, `src/store/changesets.rs`, `src/store/import.rs`, `src/store/repair.rs`, `src/overlay.rs`, `src/mcp/changesets.rs`, `src/mcp/writes.rs`, `src/http/api/changesets.rs`, `src/http/archives.rs` (import side).

- [ ] Entry points of publish, import, repair, changeset open, edit, discard and revert take `&CatalogDb` (or begin their transaction on one). Callers get it from `App::main_store()` for now.
- [ ] Where one of these transactions also reads or writes account tables (the audit write, `audit::lock_people`, the `catalogs` row lock, `catalogs.generation`), leave the statement in place, and list each one with file and line in this task's changelog under `## Notes` as "crosses in release 1". Tasks 11-13 remove every listed item. That list is the inventory those tasks work from.
- [ ] Tests: existing tests pass unchanged apart from handle types.

> **Corrected (issue #960, 2026-11-23).** Shipped: `import::import` and `repair::repair` take `&CatalogDb` (`db.begin()`); publish, `write::apply`/`apply_audited` (edits and revert) and `open_changeset`/discard in `src/mcp/changesets.rs` begin on `app.main_store()`, since they already take `App` or `ToolContext`. `CatalogDb::new` is now `pub` (not `pub(crate)` as Task 1 had it), so integration tests holding only a pool can build `CatalogDb::new(StoreName::main(), pool.clone())`; In production the server gets its handle from `App`; the CLI's `import` and `repair-indexes` (`src/cli.rs`) build `CatalogDb::new(StoreName::main(), pool.clone())` themselves in release 1, and Tasks 8 and 10 must route those two through the registry as well. Reads inside these functions (`state.refresh`, `changesets::entries`, `with_current_revisions`) still use `App::pool()` and are Task 3's. The "crosses in release 1" inventory is in `changelog/2026-11-23T12-55-43Z-m21-task-02-catalog-writes-take-catalogdb.md`; Tasks 11-13 start from it.

### Task 3: catalog reads take `CatalogDb`

**Files:** `src/catalog.rs`, `src/store/history.rs`, `src/store/documents.rs`, `src/store/export.rs`, `src/store/gates.rs`, `src/mcp/history.rs`, `src/mcp/documents.rs`, `src/mcp/search.rs`, `src/mcp/context.rs`, `src/http/api/browse.rs`, `src/cache/catalogs.rs`.

- [ ] Snapshot loads, history, revision content, export and search take `&CatalogDb`. Add to the "crosses" inventory as in Task 2.
- [ ] Tests: existing tests pass.

> **Corrected (issue #961, 2026-11-23).** Shipped:
> - `CatalogState::load`, `refresh` and `current_covering`, `OverlayCache::get`, `CatalogCache::new` (which now holds a `CatalogDb`), `history::{revisions, content_at, recent}` and `export::export` take `&CatalogDb`. `CatalogCache` is given `App`'s `main_store()` clone when `App` is built; the CLI's `export` builds its own main-store handle from its pool, like its `import`, so Tasks 8 and 10 must route that construction through the registry too.
> - The read inside the write paths that Task 2 left is typed as well: `state.refresh` in `write::apply` and `publish`, `changesets::entries` in `publish`, and `with_current_revisions` (now takes `&CatalogDb`) use `App::main_store()`. `changesets::by_handle` and `changesets::entries` in the read tools pass `main_store().executor()`.
> - `src/store/documents.rs`, `src/store/gates.rs`, `src/mcp/documents.rs` and `src/mcp/search.rs` needed no change: `documents::published` takes an executor (P2), `gates` reads only account tables, and the two tools read the in-memory snapshot and reach the database only through the files above. `CatalogDb::main_of(&pool)` (test-only) builds the release-1 handle in unit tests.
> - Still on `App::pool()`, left to later tasks: the changeset listing (`changesets::list_open`, `open_for_catalogs`, which join `catalogs` and `api_keys`: Task 5), `catalogs::row_by_id` in `ToolContext::resolve_changeset` (the registry: Task 6) and everything under maintenance (Task 4).
> - `export` writes `catalog.exported`, which P4 keeps in the account database, through the store handle in release 1 (`record_export`), and its snapshot transaction reads `catalogs`, `teams` and `audit_teams`. They must be split onto an `&AccountDb` before Task 11 moves the tables out of the store's schema. The full inventory is in `changelog/2026-11-23T13-36-45Z-m21-task-03-catalog-reads-take-catalogdb.md`.

### Task 4: maintenance takes `CatalogDb`

**Files:** `src/store/maintenance/*.rs`, `src/mcp/maintenance/*.rs`, `src/maintenance/`.

- [ ] Same treatment for runs, work, findings, evidence and candidates. Add to the "crosses" inventory, including `runs.rs:481`'s `api_keys` subselect.
- [ ] Tests: existing tests pass.

> **Corrected (issue #962, 2026-11-23).** Shipped: every public entry point in `src/store/maintenance/{runs,reports,evidence}.rs` that took `&PgPool` takes `&CatalogDb` (`runs::{list_runs, get_run, claim, renew, close}`, `reports::{get_finding, list_findings, list_findings_with, count_findings, dispose, submit}`, `evidence::read`, and the private `runs::snapshot`); the transactions begin with `CatalogDb::begin`. `candidates` and the helpers in `mod.rs` keep `&mut PgConnection` (P2). `src/mcp/maintenance/{reports,work}.rs` pass `App::main_store()`; the owner lookup in `work.rs` reads `maintenance_runs` through `main_store().executor()`. `src/maintenance/` holds no SQL and needed no change. The inventory is in `changelog/2026-11-23T14-04-24Z-m21-task-04-maintenance-takes-catalogdb.md`; it adds two items the task text did not name: `maintenance::lock` takes the catalog's advisory lock and `FOR KEY SHARE` on `catalogs` at the start of every maintenance write, and the foreign keys from all seven `maintenance_*` tables to `catalogs`. Task 11 must handle both. The `api_keys` subselect is at `runs.rs:482`.

### Task 5: the five joined statements are split

**Files:** `src/store/changesets.rs`, `src/store/repair.rs`, `src/store/users.rs`, `src/store/maintenance/runs.rs`, and their callers.

- [ ] Each statement in "Verified facts" that joins both sides becomes two statements: one on each side, joined in Rust by id. Keep result order and pagination identical.
  - `changesets.rs:654`: read the changesets from the store, then the catalog names and key labels from the account database in one `= ANY($1)` query each.
  - `users.rs:1268` and `:1494`: read the user's key ids from the account database, then the open changesets for those key ids from the store. In release 1 the store is `main_store()`; Task 13 makes this a fan-out over every store.
  - `runs.rs:482` (in `claim`; line 481 at `22c07f7`): the caller checks the key id exists on the account database first, then inserts the plain value.
  - `repair.rs:281`: read the catalog ids from the account database, then the documents.
- [ ] Tests: one test per split statement, asserting the same rows as before on a fixture with two catalogs, two keys and a deleted key: `changeset_listing_names_catalogs_and_keys_after_the_split`, `user_catalogs_with_open_changesets_after_the_split`, `deleting_a_user_discards_their_changesets_after_the_split`, `maintenance_run_records_a_missing_key_as_null`, `repair_lists_the_same_catalogs_after_the_split`.

> **Corrected (issue #963, 2026-11-23).** Shipped, with these differences from the text above:
> - `changesets::list_open` and `open_for_catalogs` take `(&AccountDb, &CatalogDb, ..)`. When the list is narrowed by catalog name, the account database first resolves the names to ids; the store then reads the changesets and paths; the account database then returns the catalogs' names (ordered by name, so the result order is the old statement's, whatever the database collation) and the key labels, one `= ANY($1)` query each.
> - `users.rs`: `delete_checked` reads the catalogs through the new private `open_changeset_catalogs`, and `discard_open_changesets` takes the user's key ids (`keys`, already read and locked by `delete_checked`) instead of joining `api_keys`. Both run on the deletion's transaction, **not on `main_store()`**: that transaction already holds the keys' locks and takes `FOR UPDATE` on the changesets, and in release 1 it reaches both sides. Each statement names one side's tables. Task 13 splits the transaction along with the store fan-out.
> - `runs::claim` takes `accounts: &AccountDb` as a new first argument and checks the key row itself, before it begins its store transaction (so no connection waits on another while the transaction holds the catalog's locks; a pool of one connection would otherwise wait on itself), rather than leaving it to the caller. In release 1 a key deleted between that read and the insert (a window spanning the transaction's lock waits and selection) fails the claim with a foreign-key error (a retry succeeds); Task 11 drops the foreign key.
> - `repair::missing_root_index` takes `(&AccountDb, &CatalogDb)`; so does the CLI's private `repair_targets`.
> - `AccountDb::of(&pool)` is the test-only account-side twin of `CatalogDb::main_of`.
> - Line numbers in the bullets above are those at `22c07f7`.

### Task 6: every remaining caller picks a side; `App::pool()` goes

**Files:** every remaining production user of `App::pool()` (HTTP API, sign-in, OAuth, OIDC, passkeys, mail, feedback, housekeeping, CLI, telemetry, invalidate).

- [ ] Account-side callers use `app.accounts()`. Remove `App::pool()` and the deprecation allowance. `grep -rn '\.pool()' src` finds only `AccountDb`/`CatalogDb` internals and tests.
- [ ] Tests: existing tests pass. The release is deployable: no schema change, no behaviour change.

> **Corrected (issue #964, 2026-11-23).** Shipped: `App::pool()` and the crate-wide `#![allow(deprecated)]` are gone. Every account-side caller passes `app.accounts().executor()` to the store-layer functions that take an executor (they take `&PgPool`, P2); `AccountDb` has no deref to the pool. Two catalog-data callers use `app.main_store().executor()`: housekeeping's `changesets::prune_empty` and the `changesets::open_total` count in `cluster_stats`. `catalogs::row_by_id` in `ToolContext::resolve_changeset` is the registry, so it is an account read. The tests were moved the same way (`test.app.accounts().executor()`), so `grep -rn '\.pool()' src` now finds only `WorkerPool::pool()` (the mail queue's own pool, `src/mail/queue.rs`). No new crossing; the inventory is unchanged from Tasks 2 to 5.

## Release 2: registry, stores and placement

### Task 7: migration 0036 (stores, placement, registry columns) and the folder move

**Files:** move `migrations/*.sql` to `migrations/accounts/`; `src/db.rs` (`migrate!("./migrations/accounts")`), `build.rs` if it names the folder, `tests/support/pg.rs` (template); create `migrations/accounts/0036_stores.sql`.

- [ ] `main` already holds migrations up to 0035 (the feedback indexes). Number the new ones from the next free version when the task starts (0036 to 0040 for this task, then 0041 and 0042 below, if nothing else has landed; see the note after the migration), and correct every reference in this plan.
- [ ] Moving the files changes nothing sqlx checks (version and checksum). Confirm with a test that a database migrated before the move reports no pending or unknown migration after it: `moving_the_migrations_folder_keeps_their_history`.
- [ ] Migrations (the files are the source; this lists what each holds):
  - `0036_stores.sql`: `stores` (name, `location_kind` `account-database` or `host`, host, port, database, `credential_kind` `iam`/`secrets-manager`/`env`, `credential_ref`, `created_at`; two table CHECKs tie the kind to the columns), the `main` row, `stores_one_account_database` and `stores_one_row_per_database` (unique on `lower(host), port, database` for host rows; Task 14 adds the check for aliases these can't see), the `default_store` setting `"main"`, and `teams.store` with `teams_store_fkey` (`ON DELETE RESTRICT`, NOT VALID);
  - `0037_catalog_placement.sql`: `catalogs.store text NOT NULL DEFAULT 'main'`, `catalogs.provisioned_at timestamptz DEFAULT now()`, and `catalogs_store_fkey` (`ON DELETE RESTRICT`, NOT VALID);
  - `0038_stores_audit.sql`: the audit target kind `store` (below), both CHECKs NOT VALID;
  - `0039_stores_validate.sql`: validates the two foreign keys and the two audit CHECKs;
  - `0040_catalogs_unprovisioned_index.sql` (`-- no-transaction`): `CREATE INDEX CONCURRENTLY IF NOT EXISTS catalogs_unprovisioned ON catalogs (created_at) WHERE provisioned_at IS NULL`, which the provisioning job finds half-created catalogs by; the name is in `CONCURRENT_INDEXES` in `src/db.rs`.

  Both `catalogs` defaults keep release-1 replicas working during the rollout (P11): their creates name neither column, and land on `main`, provisioned. New code always passes both explicitly.

> **Corrected (issue #965, 2026-11-23).** The first draft of 0036 held every statement in one transaction, so ACCESS EXCLUSIVE on `teams` and `catalogs` lasted through a backfill `UPDATE` of every catalog, the build of the partial index, and any wait for the `audit_entries` lock (review of PR #992; the repository targets a million catalogs and release 2 is a rolling deploy). It is now five migrations: each lock-taking `ALTER` is alone in its file; both foreign keys and both audit CHECKs are added NOT VALID and validated in `0039` under SHARE UPDATE EXCLUSIVE; the index is built CONCURRENTLY in its own `no-transaction` file (0025's pattern); and the backfill is gone: `provisioned_at DEFAULT now()` gives every existing row the time the migration ran (a non-volatile default is metadata-only in PostgreSQL 11 and later, `now()` is STABLE), where this plan had said `created_at`. Nothing reads `provisioned_at` as a creation time, only whether it is NULL. Later migrations move up: `0041_catalog_schema.sql` (Task 11) and `0042_catalog_audit.sql` (Task 12). The folder move is checked by `moving_the_migrations_folder_keeps_their_history`, which compares the embedded history with `tests/fixtures/migration_history_0035.txt`, taken before the move.

- [ ] `0038_stores_audit.sql` widens `audit_entries.target_kind` to include `store` and adds a `WHEN 'store'` arm to `audit_entries_target_shape` (`target_name IS NOT NULL`, the other target columns NULL), for Task 10's actions. Copy the constraint's current text from the latest migration that redefines it, so nothing else changes.
- [ ] Tests: `a_release_one_create_after_0036_lands_on_main_provisioned` (an insert naming only today's columns); `migration_0036_puts_every_catalog_on_main_provisioned` (runs 0036 and 0037 over existing catalogs); `stores_refuses_a_host_row_without_a_credential_reference`; `stores_refuses_an_account_database_row_with_a_host`; `a_second_account_database_store_is_refused`; `a_second_row_for_the_same_host_port_and_database_is_refused`.

### Task 8: `Stores`, the registry in code

**Files:** create `src/stores/registry.rs`, `src/store/stores.rs`; modify `src/app.rs`, `src/cache/directory.rs` (rows carry `store`), `src/invalidate.rs` (a `Stores` invalidation), `src/config.rs`.

- [ ] `Stores::get` reads the store's row (cached), and opens its pool on first use:
  - `account-database`: a new pool to the account database's URL;
  - `host`: host, port and database from the row, with the credential from Task 9 (`env` only until then).

  In both cases the pool sets `search_path` at connect through `after_connect`, uses `store_max_connections`, sets `min_connections = 0` and `idle_timeout = store_idle_timeout`, and gives up after `store_connect_timeout` with `StoreUnavailable`. Concurrent first calls for one store share one open, as `try_get_with` does in the caches.
- [ ] Until Task 11, the store pool's `search_path` is `public`, because the catalog tables are still there. Task 11 switches it to `catalog`. Write the value in one constant so that switch is one line.
- [ ] Directory lookups and listings hide a registry row whose `provisioned_at` is NULL. Nothing writes one yet (Task 10b does); shipping the reader first is what makes 10b safe to roll out (P11). Test: `an_unprovisioned_row_is_hidden_from_lookups_and_listings`.
- [ ] `App::catalog_db(row)` replaces `App::main_store()`; every caller from Tasks 2-6 moves to it.
- [ ] Changing a `stores` row sends an invalidation, and each replica drops that store's pool (`Stores::forget`).
- [ ] Tests: `account_database_store_opens_its_own_pool`; `concurrent_first_use_opens_one_pool`; `an_unreachable_store_is_store_unavailable_within_the_timeout` (a `host` row pointing at a closed port on `127.0.0.1`); `changing_a_store_row_reopens_its_pool_on_every_replica` (two-replica harness).

> **Corrected (issue #966, 2026-11-23).** Shipped: `Stores::new(accounts: PgPool, PoolSettings)` (`PoolSettings::from_config`), clonable, with its pools in a moka cache (single flight per store, a failed open is not kept) and `Stores::{get, all, forget, forget_all}` (`forget_all` runs when the change feed reconnects). `get` is `NotFound` for no row, `StoreUnavailable` on a failed or slow connect (logged on the local side only), `Busy` if the registry cannot be read. A store inside the account database takes the account pool's connect options; a `host` row takes them too, with host, port and database from the row and the password from the environment variable `credential_ref` names (the account database's own password is never sent). The user, TLS and other settings therefore come from the account pool's options: Task 9 decides the user's place on the row and fills in `secrets-manager` and `iam`, which are `StoreUnavailable` until then. The search path is `STORE_SEARCH_PATH` in `src/stores/registry.rs`. `App` holds `stores: Stores`; `App::catalog_db(&CatalogRow)` and `App::store_db(&StoreName)` (for a caller holding a `CatalogState`, which now has `store()`, or a changeset) replace `main_store()`. `CatalogRow` carries `store`, and `CatalogCache` holds a `Stores`. The directory queries (`row_by_name`, `row_by_id`, `rows_by_names`, `page`, `ceiling_page`) hide `provisioned_at IS NULL`; `catalogs::find` and the other by-name reads do not, so Task 10b's create reads its own half-made row by id with its own query. The invalidation is `Invalidation::Store { name }` (`Invalidation::store(&name)`); nothing writes a `stores` row yet, so Task 10 must call `invalidate(tx, &[Invalidation::store(&name)])` in every transaction that changes or removes one. The CLI `import` and `export` look up the catalog's row and open its store through the registry. Still on `main` until the task named, each marked in the code: the changeset list across catalogs (`list_changesets` without `catalog`) and `ToolContext::resolve_changeset`'s `by_handle` (Task 18: a handle does not name its store, so the lookup must try every store, `main` first), `prune_changesets` and `cluster_stats`'s open-changeset count (Task 16), and CLI `repair-indexes` (Task 20, with P8: it must loop over `Stores::all`).

### Task 9: credential kinds `secrets-manager` and `iam`

**Files:** create `src/stores/credentials.rs`; modify `Cargo.toml`.

- [ ] `env`: the reference names an environment variable holding the password; the user is the URL's or the row's. Decide the row shape for the database user name at task start (a `user` column added by this task's migration, or part of `credential_ref`), and correct this plan.
- [ ] `secrets-manager`: the reference is a secret ARN whose value is JSON with `username` and `password`, the format RDS-managed secrets use. Fetched at pool open, and again when a connection attempt fails authentication. Never logged.
- [ ] `iam`: an RDS IAM auth token for the row's host, port and user, signed from the server's AWS credentials (the `aws-config` chain already used for SES). Tokens last 15 minutes, and a pool's connect options (with the password) are what every new physical connection uses, so retiring connections early does not help. The pool's options must carry a token that is still valid whenever a connection opens: a refresh task per IAM store replaces them with a freshly signed token every 10 minutes through sqlx's `Pool::set_connect_options`, and a connection attempt that fails authentication triggers one immediate refresh and retry. Confirm `set_connect_options` in sqlx 0.9 at task start; if it is missing, wrap the pool so each physical connection is opened with a fresh token, and record which.
- [ ] Tests, with the AWS clients behind a trait and mocked: `secrets_manager_credential_is_read_from_the_secret_json`; `iam_replacement_connection_after_the_first_token_expires_uses_a_fresh_token` (a mocked signer whose tokens expire after a test-controlled interval and a test Postgres whose password changes with them: open a connection, expire the token, open another, and assert it authenticates); `iam_auth_failure_refreshes_once_and_retries`; `a_credential_never_appears_in_logs_or_errors` (captures tracing output during a failed connect).

> **Corrected (issue #967, 2026-11-23).** Decisions, and what shipped (read `src/stores/credentials.rs`):
>
> - **The database user has no column and there is no migration** (Task 11 stays 0041 and Task 12 stays 0042). Each kind says the user in its own way. `env`: `credential_ref` names the variable holding the password, and the user is the account pool's own user (host, port and database come from the row; user, TLS and the rest from the account pool's options, as Task 8 shipped). `secrets-manager`: `credential_ref` is the secret ARN, and the secret's `username` is the user. `iam`: `credential_ref` is the database user the token is signed for. Task 10's add-store check and Task 19's docs must describe `credential_ref` this way.
> - **IAM signing is `aws-sigv4` directly, not `aws-sdk-rds`:** the token is one SigV4 presigned `GET https://HOST:PORT/?Action=connect&DBUser=USER` for service `rds-db`, valid 900 seconds, without the scheme (`presign_token`). The whole RDS SDK crate would add only that call. Region: the RDS host name's when it has the RDS shape (`name.id.REGION.rds.amazonaws.com`), because RDS rejects a token signed for another region; the SDK chain's region only for a host name that does not say (a proxy, a CNAME). The row has no region field and no migration is added, so there is no per-store override; a database behind a custom name signs for the server's region (review round, 2026-11-23). `aws-sdk-secretsmanager` reads the secret, in the region the ARN names.
> - **`Pool::set_connect_options` exists in sqlx 0.9** (`sqlx-core-0.9.0/src/pool/mod.rs:555`; `connect_options()` at `:545`). It replaces the options behind a lock that every new physical connection reads (`pool/inner.rs:342-350`), and leaves open connections alone, so no pool wrapper is needed.
> - **Shape.** Credential resolution left `registry.rs` for `credentials.rs`: trait `CredentialSource` (`fetch`, `refresh`) with `EnvCredential`, `SecretsManagerCredential` and `IamCredential`; the AWS clients are behind `SecretReader` and `TokenSigner` and mocked in tests; `Providers` builds a source from a row (`Stores::with_providers` replaces `Stores::with_environment` as the injection point, which stays as a test helper). The real clients share one lazily loaded `aws_config::defaults` chain, the one SES uses.
> - **Refresh.** `Refresh::Never` (`env`), `OnAuthFailure` (`secrets-manager`), `Every(10 minutes)` plus `OnAuthFailure` (`iam`). A refused login (SQLSTATE class 28) while opening is refetched and retried once; a `Never` source is tried once. After the open, one task per pool with a changing credential (`spawn_keeper`) replaces the pool's options on the timer, and when a refused login is seen. sqlx has no hook on a pool's connect path and most store reads reach the pool through `CatalogDb::executor()` with `?`, so the signal is raised where every driver error passes: `CatalogDb::classify` (which also returns `StoreUnavailable`) and `OkfError::from(sqlx::Error)` call `signal_auth_refused`, which broadcasts the role the server's message names; each task answers when that role is its pool's current login (or the message names none), at most one refresh per 10 seconds. The ~48 `executor()` call sites are untouched (follow-up #995 moves them onto `classify`). Review round, 2026-11-23. The task belongs to the registry entry: `Stores::forget` and `forget_all` drop the entry and the task ends, then the pool closes with its last handle.
> - **Not done:** a request that is refused a login still fails (`StoreUnavailable`); only the next one succeeds. Retrying inside the pool would need a pool wrapper, which the plan said to build only if `set_connect_options` were missing. This is an accepted limitation, not transparent recovery: initial opening retries, but an already-open pool fails the triggering operation, and even the next request can race the asynchronous refresh. Follow-up #998 covers a bounded acquisition retry with a regression test on an open pool (review round, 2026-11-23).

### Task 10: placement and the admin surface

**Files:** create `src/stores/placement.rs`, `src/http/api/stores.rs`, `src/cli/stores.rs`; modify `src/store/catalogs.rs` (create), `src/store/teams.rs`, `src/store/audit.rs` (actions).

- [ ] `placement::choose(accounts, owner_team) -> StoreName`: the owner team's pin if set, else the `default_store` setting.
- [ ] Admin API, admin credentials only:
  - `GET /api/v1/stores` and `POST /api/v1/stores` (add a row);
  - `PUT /api/v1/settings/default_store`;
  - `PUT` and `DELETE /api/v1/teams/{team}/store` (pin and unpin).

  The CLI has the same commands under `okf-mcp stores`. Adding a store first checks it can connect, and refuses otherwise. Until Task 20, adding a store with location `host` is also refused with a message saying separate stores aren't enabled yet (P8). The check is one function, so Task 20 can remove it in one place. A store that is the default or pinned by a team cannot be removed, and no removal endpoint is built (YAGNI).
- [ ] New audit actions: `store.added`, `setting.default_store_changed`, `team.store_pinned`, `team.store_unpinned`. `target_kind` gains `store`, with `target_name` set to the store's name; migration 0036 (Task 7) widens the checks for it.
- [ ] Tests: `a_new_catalog_lands_on_the_default_store`; `a_team_pin_wins_over_the_default`; `changing_the_default_moves_new_catalogs_only`; `adding_an_unreachable_store_is_refused`; `adding_a_host_store_is_refused_until_enabled`; `store_admin_endpoints_refuse_non_admin_credentials`; `every_store_change_is_audited`.

> **Corrected (issue #968, 2026-11-23).** Shipped: `placement::choose(conn: &mut PgConnection, owner_team: &str)` takes the creating transaction's connection (not an account handle), so the choice and the insert read one snapshot; `catalogs::create` calls it and writes `store` and `provisioned_at` (both explicit, `provisioned_at` = the creation time until Task 10b). `placement::default_store` reads the `default_store` setting (`main` if the row is missing). The writes are in `src/store/stores.rs` (`add`, `set_default`, `summaries`) and `src/store/teams.rs` (`set_store`, `None` unpins), not in `placement.rs`. `Stores::check(row)` is the connection check: the registry's own open path (`open_row`, shared with `get`), run on a row that is not stored yet, with its pool closed again. `add` runs the check first and holds no transaction, then the P8 refusal, then `insert` (row, audit and `Invalidation::store`, one transaction); so an unreachable `host` store is refused as unreachable, and a reachable one as "not enabled". The P8 refusal is the single function `placement::refuse_host_stores_until_task_20`, called once, from `add`. Invalidations: `Invalidation::Store` for a new row, `Invalidation::Settings` for the default (the setting is read straight from the database, so nothing else is needed), `Invalidation::Team` for a pin. Audit: `store.added` has target kind `store`; `setting.default_store_changed` targets the setting `default_store` (details `from`, `to`), and `team.store_pinned` / `team.store_unpinned` target the team (details `from`, `to`), so `store` is the target kind of `store.added` alone. No migration was needed (0038 had it). The routes are `GET`/`POST /stores`, `PUT /settings/default_store` (body `{value}`, like the other settings) and `PUT`/`DELETE /teams/{team}/store` (body `{store}`); writes need an admin reaching every catalog, and none is in `PROVISIONER_ROUTES`. CLI: `stores list`, `add`, `set-default`, `pin`, `unpin`. `stores` is not in the parity test's scanned groups, so no console control was built; docs are in `docs/operations.md` ("Catalog stores API").

### Task 10b: creating a catalog in two steps

**Files:** `src/store/catalogs.rs` (create), `src/housekeeping.rs` (new task `ProvisionCatalogs`).

- [ ] Task 8 already makes every lookup and listing hide a row whose `provisioned_at` is NULL; this task is the first to write one. It ships as its own deploy (release 2b) after release 2 is on every replica, so no replica that shows unprovisioned rows is still running (P11).
- [ ] Create inserts the registry row with `store` from placement and `provisioned_at` NULL, and commits. Then it runs the store transaction under P9's protocol: take the catalog's lock on the store, check the registry row on the account database, write the store side, commit. Then it sets `provisioned_at`. Directory lookups and listings treat a row with `provisioned_at` NULL as absent.
- [ ] **What "the store side" is depends on the release.** In release 2 it is the starter files (`documents` and `revisions` rows), since `catalog_state` doesn't exist yet and the only store is `main` (P8). From Task 11 on, it also includes the `catalog.catalog_state` row; Task 11 updates create and recovery to write and check it. Write the existence check as one function, `store_side_exists(catalog_db, id)`, so Task 11 changes one place.
- [ ] `ProvisionCatalogs` runs every minute over unprovisioned rows older than one minute (an efficiency filter only; safety comes from P9's lock, not the age). For each row it follows P9's recovery half, and records a failed create in the audit details when it removes a row.
- [ ] Tests: `a_half_created_catalog_is_not_visible`; `provisioning_job_finishes_a_catalog_whose_store_side_exists`; `provisioning_job_removes_a_catalog_whose_store_side_never_happened`; `a_create_stalled_past_the_interval_is_not_removed_while_it_holds_the_lock` (hold a create's store transaction open with a barrier, run recovery concurrently, release, and assert one consistent outcome); `a_create_that_waited_behind_recovery_writes_nothing` (recovery deletes the row first, and the create then aborts with no store rows).

Release 2b is deployable here (rolling, after release 2 is on every replica): every catalog is still on `main`, and separate stores are refused (P8).

## Release 3: the split

### Task 11: migration 0041 (schema `catalog`, `catalog_state`, foreign keys) and the lock changes

**Files:** create `migrations/accounts/0041_catalog_schema.sql`; modify `src/store/publish.rs`, `src/store/repair.rs`, `src/catalog.rs`, `src/store/changesets.rs`, `src/store/catalogs.rs`, `src/stores/registry.rs` (search_path to `catalog`).

- [ ] Migration (constraint names below are Postgres's defaults; confirm with `\d` on a migrated database at task start and correct here):

```sql
CREATE SCHEMA catalog;

-- The catalog's own state row: the publish counter, and the row writes
-- lock to hold off deletion (replaces the catalogs row in those roles).
CREATE TABLE catalog.catalog_state (
    id         bigint PRIMARY KEY,
    generation bigint NOT NULL DEFAULT 0,
    -- Set by a delete under the row lock (P12); every writer refuses a
    -- catalog_state row that has it.
    deleted_at timestamptz
);
INSERT INTO catalog.catalog_state (id, generation)
    SELECT id, generation FROM catalogs;

ALTER TABLE documents               SET SCHEMA catalog;
ALTER TABLE revisions               SET SCHEMA catalog;
ALTER TABLE changesets              SET SCHEMA catalog;
ALTER TABLE changeset_entries       SET SCHEMA catalog;
ALTER TABLE maintenance_state       SET SCHEMA catalog;
ALTER TABLE maintenance_runs        SET SCHEMA catalog;
ALTER TABLE maintenance_work        SET SCHEMA catalog;
ALTER TABLE maintenance_findings    SET SCHEMA catalog;
ALTER TABLE maintenance_evidence    SET SCHEMA catalog;
ALTER TABLE maintenance_dispositions SET SCHEMA catalog;
ALTER TABLE maintenance_requests    SET SCHEMA catalog;

-- Catalog tables now reference catalog_state, not the registry.
ALTER TABLE catalog.documents DROP CONSTRAINT documents_catalog_id_fkey,
    ADD FOREIGN KEY (catalog_id) REFERENCES catalog.catalog_state (id) ON DELETE CASCADE;
ALTER TABLE catalog.revisions DROP CONSTRAINT revisions_catalog_id_fkey,
    ADD FOREIGN KEY (catalog_id) REFERENCES catalog.catalog_state (id) ON DELETE CASCADE;
ALTER TABLE catalog.changesets DROP CONSTRAINT changesets_catalog_id_fkey,
    ADD FOREIGN KEY (catalog_id) REFERENCES catalog.catalog_state (id) ON DELETE CASCADE;
-- (the same for each maintenance_* table that references catalogs)

-- Foreign keys across the line become plain values (spec Decision 9).
ALTER TABLE catalog.changesets DROP CONSTRAINT changesets_key_id_fkey;
ALTER TABLE catalog.maintenance_runs DROP CONSTRAINT maintenance_runs_key_id_fkey;

-- The maintenance audit triggers read and write account tables, so they
-- cannot follow maintenance_runs into the store (review of PR #956).
-- Task 11 replaces them with a write from Rust; Task 12 moves that write
-- into the store transaction.
DROP TRIGGER maintenance_runs_audit_end ON catalog.maintenance_runs;
DROP TRIGGER maintenance_runs_audit_empty ON catalog.maintenance_runs;
DROP FUNCTION audit_maintenance_run();

ALTER TABLE catalogs DROP COLUMN generation;
-- Deleted but not yet cleaned up (P6).
ALTER TABLE catalogs ADD COLUMN deleted_at timestamptz;

-- Cleanup owed on stores after an account-side deletion (P13), written in
-- the deleting transaction. One row per subject per store; the ids are
-- kept here because the key or user they name is gone.
CREATE TABLE store_cleanups (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    store      text NOT NULL REFERENCES stores (name) ON DELETE RESTRICT,
    kind       text NOT NULL CHECK (kind IN ('key', 'catalog')),
    subject    text NOT NULL,
    created_at timestamptz NOT NULL,
    attempts   integer NOT NULL DEFAULT 0,
    UNIQUE (store, kind, subject)
);
```

- [ ] A test asserts no foreign key from schema `catalog` references schema `public`, and the reverse: `no_foreign_key_crosses_between_schemas`. It reads `pg_constraint`, so a missed constraint fails it.
- [ ] Code:
  - `generation` is read and bumped on `catalog.catalog_state`;
  - `changesets::lock_catalog` takes `FOR KEY SHARE` on `catalog_state`;
  - publish's advisory lock is unchanged, since catalog ids are unique;
  - the store pool's `search_path` becomes `catalog`;
  - create and `ProvisionCatalogs` write and check `catalog_state` (Task 10b's `store_side_exists`);
  - the two maintenance audit paths the dropped triggers covered (a run reaching an end state, and a run inserted already ended with nothing to do) record `catalog.maintenance_run` from Rust. They resolve the credential and team ids on the account database first, then write the entry with the same details the trigger wrote (`findings` count, outcome). In this task the entry goes to the account `audit_entries` in its own transaction; Task 12 moves it into the store transaction. Tasks 11-17 deploy together (P10), so the interim state never reaches production.

  Remove every "crosses" item from Tasks 2-4 that this covers, and mark it done in the inventory.
- [ ] Tests: `maintenance_run_end_is_audited_after_the_trigger_is_gone`; `an_empty_maintenance_run_is_audited_after_the_trigger_is_gone`; `publish_bumps_generation_in_catalog_state`; `a_store_pool_cannot_see_account_tables` (a query for `users` on a store pool fails); `an_account_pool_cannot_see_catalog_tables`.

### Task 12: catalog audit on the store

**Files:** create `migrations/accounts/0042_catalog_audit.sql`; modify `src/store/audit.rs`, `src/write.rs`, `src/store/publish.rs`, `src/store/import.rs`, `src/store/repair.rs`, `src/http/api/audit.rs`, `src/http/api/audit/cursor.rs`, `src/cli/audit.rs`.

- [ ] Migration:

```sql
-- Catalog-event audit entries (spec Decision 10, P4). Identity columns are
-- the account database's audit_people / audit_credentials / audit_teams ids
-- as plain values: no foreign keys across the line.
CREATE TABLE catalog.catalog_audit_entries (
    id                    bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at                    timestamptz NOT NULL,
    catalog_id            bigint NOT NULL,
    action                text NOT NULL CHECK (action ~ '^[a-z]+\.[a-z_]+$'),
    source                text NOT NULL CHECK (source IN ('cli', 'api', 'console', 'system')),
    actor_credential_ref  bigint,
    actor_person_id       bigint,
    actor_label           text NOT NULL,
    actor_operator        boolean NOT NULL,
    client_ip             inet,
    target_name           text NOT NULL,
    teams                 bigint[] NOT NULL DEFAULT '{}',
    details               jsonb NOT NULL DEFAULT '{}'::jsonb
                              CHECK (jsonb_typeof(details) = 'object')
);
CREATE INDEX catalog_audit_entries_at ON catalog.catalog_audit_entries (at);
CREATE INDEX catalog_audit_entries_teams ON catalog.catalog_audit_entries USING gin (teams);

-- Append-only, as the account table is. A store has no server_settings, so
-- the prune job passes the retention period in a transaction-local setting
-- (SET LOCAL okf.audit_retention_days); without it, 400 days. Never less
-- than 30, whatever is passed.
CREATE FUNCTION catalog.catalog_audit_entries_append_only() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    retention integer;
BEGIN
    IF TG_OP <> 'DELETE' THEN
        RAISE EXCEPTION 'audit entries are append-only' USING ERRCODE = 'insufficient_privilege';
    END IF;
    retention := GREATEST(30, COALESCE(
        NULLIF(current_setting('okf.audit_retention_days', true), '')::integer, 400));
    IF OLD.at >= now() - make_interval(days => retention) THEN
        RAISE EXCEPTION 'audit entry % is inside the retention period', OLD.id
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER catalog_audit_entries_append_only BEFORE UPDATE OR DELETE ON catalog.catalog_audit_entries
    FOR EACH ROW EXECUTE FUNCTION catalog.catalog_audit_entries_append_only();
CREATE TRIGGER catalog_audit_entries_no_truncate BEFORE TRUNCATE ON catalog.catalog_audit_entries
    FOR EACH STATEMENT EXECUTE FUNCTION catalog.catalog_audit_entries_append_only();

-- Entries written before the split stay in the account table (P7).
```

  Entries written before the split are not moved (P7). The view reads both tables, so they still appear, exactly once.
- [ ] Write path: before a catalog transaction begins, the caller resolves the actor's `audit_people`, `audit_credentials` and `audit_teams` ids on the account database (today's `audit::lock_people` / `audit_person()` work, in its own account transaction). It passes them in; the catalog transaction inserts into `catalog.catalog_audit_entries`. `audit::record` gains a sibling `audit::record_catalog(conn, &ResolvedActor, &CatalogEntry, now)`. The P4 write sites switch to it, including Task 11's maintenance-run write, which now runs in the store transaction that ends the run.
- [ ] Read path: the owner and admin views query the account database and every store in `Stores::all()`, concurrently, with the same `teams` filter visibility uses today (`src/store/audit.rs:1856`), so entries stay visible after their catalog is deleted or transferred, each with the same filter and limit. They merge by `(at, store, id)` descending. The cursor encodes `(at, store, id)`, with the account database as store `""`, and stays signed and opaque as today. Retention pruning covers both tables (Task 16).
- [ ] Tests: `a_publish_writes_its_audit_entry_on_the_store`; `a_failed_publish_writes_no_audit_entry`; `entries_from_before_the_split_appear_once_in_the_view`; `store_audit_refuses_update_and_recent_delete`; `store_audit_prune_honours_the_passed_retention_and_its_floor`; `audit_view_pages_across_two_stores_and_accounts_without_gaps_or_repeats`; `audit_view_keeps_entries_after_the_last_catalog_on_a_store_is_deleted`; `audit_view_keeps_entries_after_a_catalog_is_transferred`; `forgetting_a_person_clears_their_name_in_store_entries_too` (the view resolves names through the account database).

### Task 13: deletion in two steps, and cleanup on every store

**Files:** `src/store/catalogs.rs` (delete), `src/store/changesets.rs` (`lock_catalog` checks `deleted_at`), `src/store/users.rs`, `src/store/keys.rs`, `src/store/teams.rs`, `src/housekeeping.rs` (new task `CleanupStores`), `src/cache/directory.rs`.

- [ ] Catalog delete (P12). One store transaction stays open around the account transaction:
  1. On the store: take the catalog's advisory lock and `catalog_state` `FOR UPDATE`. Changeset writers take `FOR KEY SHARE` on that row, so new ones wait and in-flight ones have finished.
  2. Still on the store: unless `force`, refuse if there are open changesets with entries, as today; with `force`, discard them in this transaction. Set `catalog_state.deleted_at`.
  3. On the account database, in its own transaction: set `catalogs.deleted_at`, record `catalog.deleted`, write the `store_cleanups` row (P13), commit. The catalog is gone from lookups.
  4. Commit the store transaction. If step 3 failed, roll it back instead, and nothing changed.

  A crash between 3 and 4 leaves the store side unmarked, but the registry row is deleted and cleanup removes the data anyway. A writer that waited in step 1 gets the row after commit, sees `deleted_at`, and fails with `NotFound`. Every write path checks `catalog_state.deleted_at` after taking its lock (one helper, `changesets::lock_catalog`).
- [ ] `CleanupStores` drains `store_cleanups` per store. For a catalog it deletes the `catalog_state` row, cascading to the catalog's data but not `catalog.catalog_audit_entries` (P5), then the registry row. The name is reusable only after that (P6). It also rescans `catalogs.deleted_at` rows with no pending cleanup, as a backstop.
- [ ] Key, user and team deletion (P13): the deleting account transaction writes one `store_cleanups` row per affected key per store in `Stores::all()`, in the same transaction, so a crash after commit can't lose the cleanup. A user's or team's deletion writes rows for each of its keys, read before they are deleted. `CleanupStores` discards those keys' open changesets on each store. A store that can't be reached is retried (`attempts` counts tries; a row past 20 attempts is logged at error level each run and kept), and the deletion itself never waits on it.
- [ ] Tests: `a_deleted_catalog_disappears_before_its_store_data_is_gone`; `cleanup_removes_the_store_data_then_the_registry_row`; `a_deleted_catalogs_name_is_reserved_until_cleanup`; `catalog_audit_survives_catalog_deletion`; `deleting_a_key_discards_its_changesets_on_every_store`; `cleanup_retries_an_unreachable_store_without_blocking_others`; `an_edit_racing_a_delete_either_lands_first_and_blocks_it_or_fails_after` (barrier between delete's steps 1 and 3, an edit started meanwhile); `a_delete_without_force_never_discards_work_written_during_it`; `a_key_deletion_crashing_after_commit_still_cleans_up_its_changesets` (drop the connection after commit, before the job runs, then run `CleanupStores`).

### Task 14: two migrators, the catalog baseline, adoption, `migrate-stores`

**Files:** create `migrations/catalog/0001_baseline.sql`; modify `src/db.rs`, `src/stores/registry.rs`, `src/cli/stores.rs`, `tests/support/pg.rs`.

- [ ] `CATALOG_MIGRATOR` is `migrate!("./migrations/catalog")` with its history table set to `catalog._sqlx_migrations` (`dangerous_set_table_name`). `db::migrate_store(conn)` runs it under the existing advisory lock, after creating schema `catalog` if absent.
- [ ] The baseline is the `catalog` schema exactly as migrations 0041 and 0042 leave it: `catalog_state`, the moved tables with their indexes and constraints, and `audit_entries`. Generate it from a schema-only dump of a migrated database, then edit it into readable SQL.
- [ ] Adoption: when `catalog.catalog_state` exists and `catalog._sqlx_migrations` has no rows, insert the baseline's history row (version, description, checksum from the embedded migration, `success = true`) instead of running it, in the same locked connection.
- [ ] **Store identity.** The catalog baseline adds `catalog.store_identity (name text PRIMARY KEY)`, holding one row: the name of the store the database belongs to. Migrating or adopting writes it if absent (`main` on the account database at adoption). Adding a store connects to it, migrates it, then refuses if its `store_identity` row names another store, or if the target holds the account tables (`public.stores` exists), which means it is the account database under another name. `Stores::get` checks the row on first open and refuses a mismatch with `StoreUnavailable`, logged with both names. Each physical database therefore answers to one store name, whatever host name reached it.
- [ ] `Stores::get` migrates a store the first time its pool opens in this process. `okf-mcp stores migrate` migrates every store and reports each one's result.
- [ ] Tests: `adopted_schema_equals_a_fresh_store_schema` (compares `pg_dump --schema-only --schema=catalog` of both, after removing the history table's rows, or compares the information_schema and `pg_indexes` contents); `adoption_runs_once_under_concurrent_starts`; `a_fresh_store_gets_the_baseline`; `migrate_stores_reports_an_unreachable_store_and_continues`; `adding_a_store_that_aliases_another_is_refused` (a second host name for an already registered database); `adding_the_account_database_as_a_host_store_is_refused`.

### Task 15: the test harness runs catalog tests on a separate store

**Files:** `tests/support/pg.rs`, `src/db.rs` (`test_db`), `tests/support/server.rs`, the justfile or `tools/agentic/gate.sh` if a new mode needs a flag.

- [ ] `TestDb` gains a store mode: `OKF_TEST_STORE=same` (default; the store is the account database) or `separate` (a second database copied from a store template, registered as `main` with location `host` and credential kind `env`).
- [ ] `gate.sh test` runs the suite once in each mode. If that doubles the gate's run time beyond its timeout, run the catalog-heavy test binaries in `separate` mode only, and record the choice here.
- [ ] Tests: the existing suite in both modes; `isolation_a_catalog_on_store_b_writes_nothing_to_store_a`.

Release 3 is not deployable yet: Task 17 (listeners per store) completes it.

## After the split

### Task 16: per-store housekeeping

**Files:** `src/housekeeping.rs`.

- [ ] `PruneChangesets`, the catalog half of `PruneAudit`, and the open-changeset count in `ClusterStats` run per store: one job per task, store and slot, keyed `"<task>:<store>:<slot>"`, scheduled for each store in `Stores::all()`. `ClusterStats` sums the per-store counts into `cluster_stats`. The store's audit prune runs `SET LOCAL okf.audit_retention_days` with the account setting's value before its DELETE (Task 12's trigger). Payloads carry the store name only.
- [ ] Tests: `per_store_housekeeping_runs_once_per_store_and_slot_across_replicas`; `one_store_failing_does_not_hold_up_the_others`; `cluster_stats_sums_open_changesets_across_stores`.

### Task 17: cache signalling per store (completes release 3)

**Files:** `src/invalidate.rs`, `src/stores/registry.rs`.

- [ ] Opening a store's pool also starts a listener on that store, so catalog invalidations sent by store transactions reach every replica. The listener stops when the pool is forgotten. A lost listening connection drops the caches for that store's catalogs only.
- [ ] Release 3 deploys after this task, following P10's procedure.
- [ ] Tests (a release gate for release 3): `a_publish_on_a_separate_store_reaches_the_other_replica` (two-replica harness, separate store mode); `a_lost_store_listener_drops_only_that_stores_catalogs`.

### Task 18: reads across stores, and `unavailable`

**Files:** `src/mcp/context.rs`, `src/mcp/search.rs`, `src/mcp/history.rs`, `src/mcp/documents.rs`, `src/mcp/catalogs.rs`, `src/http/api/search.rs`, `src/http/api/browse.rs`, `docs/reference/tools.md` (field only; prose in Task 19).

- [ ] Merging reads (`search`, `list_catalogs` counts, the console search) query stores concurrently and add `unavailable: [catalog id]` when any store fails with `StoreUnavailable`. Omit the field when empty, as other optional fields are.
- [ ] First-match reads follow P3.
- [ ] `ToolContext::resolve_changeset` finds a handle's store (every store from `Stores::all()`, `main` first; an unreachable one is `StoreUnavailable` as P3), and `list_changesets` without `catalog` fans out over the stores. Both read `main` only after Task 8 (issue #966).
- [ ] Tests: `search_returns_other_stores_results_and_names_the_unavailable_catalog`; `get_concept_fails_when_an_earlier_catalogs_store_is_unavailable`; `a_write_to_an_unavailable_store_fails_with_store_unavailable`.

### Task 19: docs

**Files:** `docs/operations.md`, `docs/reference/tools.md`, the admin API reference, `README.md` if it describes the database.

- [ ] Operations:
  - what a store is;
  - adding one (credential kinds, with placeholder ARNs and hosts);
  - the default store and team pins;
  - `okf-mcp stores migrate`;
  - connection arithmetic: (1 account pool + stores opened) × replicas × pool size;
  - the release-3 upgrade procedure (P10): snapshot, scale to zero, migrate, start;
  - what a separate store does and doesn't segregate (spec "Privacy and security").
- [ ] Tools reference: `unavailable`, and the first-match behaviour.

### Task 20: end-to-end test, smoke test, milestone changelog

**Files:** create `tests/stores.rs`; `changelog/`.

- [ ] `tests/stores.rs`:
  1. Start a server with the account database and two stores.
  2. Pin team A to store `acme`.
  3. Create catalogs for team A and for team B.
  4. Run open, edit, publish, history, revert, search across both, export and maintenance.
  5. Assert each catalog's rows exist only on its own store.
  6. Stop store `acme`'s database; assert team B is unaffected and team A gets `StoreUnavailable`.
- [ ] Smoke: deploy release 3 to a disposable environment built from a snapshot of a pre-split database. Follow P10's procedure. Check the adoption, the audit view (old and new entries) and a publish, and record the result in the milestone changelog.
- [ ] CLI `repair-indexes` loops over `Stores::all()`; after Task 8 it repairs `main` only (issue #966).
- [ ] Remove P8's restriction on `host` stores once the end-to-end test passes: delete `placement::refuse_host_stores_until_task_20` and its one call in `store::stores::add` (Task 10 marks it), and invert its test: `adding_a_host_store_is_refused_until_enabled` becomes `a_host_store_can_be_added`. Update "Catalog stores API" in `docs/operations.md`, which says host stores are refused.
- [ ] Milestone changelog summarising the epic.

## Self-review

- **Spec coverage:**
  - Decisions 1-4: Tasks 7-9.
  - Decision 5 / P1: Tasks 8 and 11.
  - Decision 6: Task 10.
  - Decision 7 / P2: Tasks 1-6 and 11.
  - Decision 8: Tasks 2 and 12.
  - Decision 9: Tasks 5 and 11.
  - Decision 10 / P4 (spec 16): Task 12.
  - Decision 11: Tasks 10b and 13.
  - Decision 12: Tasks 10b and 11 (ids stay registry-allocated; `catalog_state.id` takes the registry's id).
  - Decision 13 / P3: Task 18.
  - Decision 14: Tasks 10b, 13 and 16.
  - Decision 15: Tasks 11 and 14.
  - P5 (spec 17): Tasks 12-13. P6 (spec 18): Task 13.
  - P7: Task 12. P8: Tasks 10 and 20. P9: Task 10b. P10: Tasks 17, 19 and 20. P11: Tasks 7, 8 and 10b. P12-P13: Tasks 11 and 13.
  - Spec testing section: Tasks 10, 10b, 12-18 and 20. Docs: Task 19.
- **Placeholders:** the plan leaves three choices to task start, each with the decision to record: the database user's place on the row (Task 9, decided: see its note), the IAM token refresh mechanism (Task 9, decided: see its note), and the constraint names (Task 11). No step says "handle errors" without naming them.
- **Names:** `AccountDb`, `CatalogDb`, `StoreName`, `Stores::{get, all, forget}`, `App::{accounts, stores, catalog_db}`, `OkfError::StoreUnavailable`, `audit::record_catalog`, and the housekeeping tasks `ProvisionCatalogs` and `CleanupStores` are used with the same spelling throughout. `App::main_store()` exists only between Tasks 1 and 8.

## Open questions for the maintainer

1. **Gate time.** Running the suite in both store modes may double `gate.sh test`. Is that acceptable, or should `separate` mode run only in the milestone PR's gates? Task 15 decides by default if no answer comes.
2. **Adding a store in production** needs network reach and a database user on the target cluster. That infrastructure work is outside this repo and isn't a task here.

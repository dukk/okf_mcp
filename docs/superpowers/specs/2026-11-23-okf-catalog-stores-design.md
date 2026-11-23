# okf-mcp catalog stores: design

Status: designed 2026-11-23; epic and sub-issues to be filed.

## Goal

A catalog's data can live in a database other than the account
database. Each such database is a **store**. The server routes every
catalog read and write to the catalog's store.

This epic is done when:

- account data (users, teams, keys, OAuth, the catalog registry, account
  audit) is separated from catalog data (documents, history, changesets,
  maintenance, catalog audit);
- a new catalog can be created on any registered store, and works end to
  end there;
- the test suite runs every catalog path with the store sharing the
  account database and with the store in a separate database.

Production keeps running on one database: the account database and one
store, `main`, which lives inside it. Nothing moves until an operator adds
a second store.

## Why now

1. **Scale.** One Aurora cluster has one writer instance and a storage
   ceiling, and its restore time and blast radius grow with every user.
   Catalogs are already almost independent (every catalog-data query is
   scoped to one catalog id), so a catalog is the natural unit to place.
2. **Segregation.** Some customers will want their catalogs in a database
   no other customer's data is in.
3. **Cost of waiting.** Today 223 non-test call sites use the one pool
   (`App::pool`, `src/app.rs:535`), catalog ids are a per-database identity
   column, and catalog tables have foreign keys into account tables. Each
   new feature adds to that. Separating them is mechanical now and gets
   harder with every release.

## Non-goals

- **Moving an existing catalog between stores.** It needs a copy of the
  catalog's full history, which the revision storage redesign (a separate
  epic) changes. Today's import keeps only one `import` revision per file,
  so it is not a move. Moving a whole store (its database) is a database
  copy and is possible without this, but needs no code here.
- **Changing how revisions are stored** (deltas, content addressing, object
  storage). Separate epic.
- **Dedicated cells** (a full separate deployment per customer). Nothing
  here prevents it; a cell is another deployment with its own account
  database.
- **Per-store connection budgets, store states (draining, read-only),
  catalog freezes.** Not needed until moves or a pooler force the question.
- **Running a second store in production.** The capability ships; using it
  is an operator decision.

## Decisions

1. **A store is one Postgres database.** It may share a server (or the
   account database itself) with other stores. The registry never names a
   host or database; it names a store.
2. **Stores are rows, not config.** A `stores` table in the account
   database holds: name, location, credential reference. Config holds only
   the account database's URL, which the server needs to start. Adding a
   store is an audited admin API or CLI call, with no redeploy.
3. **A store's location is either `account-database` or a host, port and
   database name.** `account-database` means "use the account database's
   connection", which is what production and every single-database
   install use.
4. **No secrets in the stores table.** A credential reference is one of
   three typed kinds: Aurora IAM authentication (a short-lived token from
   the server's role; no password exists), a Secrets Manager secret ARN, or
   the name of an environment variable (for installs without AWS). There is
   no column a password could be written to.
5. **Pool size is server config, not a store property.** One setting, the
   maximum pool size per store, the same on every replica. Pools open on
   first use and close idle connections. Two stores with the same location
   share one pool.
6. **Placement.** A new catalog goes to its owner team's pinned store if
   the team has one, else to the current default store. Both live in the
   account database and change at runtime through the admin API or CLI,
   with an audit entry. Changing the default affects new catalogs only;
   each catalog's store is recorded on its registry row at creation.
7. **Account database and stores are distinct types.** `AccountDb` and
   `CatalogDb` wrap the pool. Store-layer functions take the one they need,
   so a catalog query on the account pool (or the reverse) does not
   compile.
8. **One write, one store transaction.** An edit, publish, revert, import
   or delete of a document runs in one transaction on the catalog's store,
   including its catalog audit entry. Account reads a write needs (access
   checks, identity ids for the audit entry) happen first, on the account
   database.
9. **No foreign keys across the line.** Catalog-store rows hold account ids
   as plain values, as `revisions.actor` already does. Integrity the
   database enforced (for example `changesets.key_id` to `api_keys`) is
   checked in code, and deletions clean up through jobs (Decision 11).
10. **Catalog audit lives on the catalog's store.** An audit entry written
    inside a catalog-store transaction goes to that store's
    `catalog.audit_entries`. Every other entry (sign-ins, keys, teams,
    settings, unlocks, read-access counts) stays in the account database.
    Catalog entries refer to people, credentials and teams by the ids of
    the account database's `audit_people`, `audit_credentials` and
    `audit_teams` rows, as plain values. Forgetting a person still clears
    their name in one place.
11. **Two-database operations: account side first, then the store, then a
    retrying job tidies up.**
    - Create: the registry row is inserted with `provisioned_at` NULL
      (allocating the id); the store's state row and starter files are
      written; `provisioned_at` is set. Until then the catalog is not
      found.
    - Delete: the registry row is marked deleted (the catalog disappears at
      once); a job deletes its store data, then the row.
    - Deleting a key, user or team queues cleanup on each store holding
      rows for it (for example its open changesets).
12. **Catalog ids come from the registry.** Store-local ids (changeset row
    ids, catalog audit entry ids) may repeat across stores and are always
    paired with a catalog or a store. Changeset handles are already random
    and server-issued.
13. **Reads across catalogs fan out and report what they could not read.**
    A call reading several catalogs (for example `search` over the
    selection) queries their stores concurrently and merges as today. If a
    store cannot be reached, the call returns the rest and an `unavailable`
    list naming the catalogs it skipped. A write to an unreachable store
    fails with an error that says so.
14. **One central job queue, in the account database.** Stores hold no job
    tables and run no workers. Per-store housekeeping (empty-changeset
    pruning, catalog audit pruning, open-changeset counts) is scheduled as
    one job per task, per store, per slot, keyed `"<task>:<store>:<slot>"`,
    and runs on that store's pool. Job payloads carry ids, never catalog
    content. A future catalog write that must start a job atomically writes
    an outbox row on its store, and a central job moves it into the queue.
15. **Catalog tables live in a `catalog` schema**, with migration history in
    `catalog._sqlx_migrations` (the job queue already uses schema `apalis`
    this way, `src/db.rs:96`). Account tables stay in `public` with the
    existing history. A store that shares the account database has both
    schemas; a separate store has only `catalog`.

## Architecture

### What goes where

| Account database (`public`) | Each store (`catalog`) |
|---|---|
| `users`, `teams`, `team_members`, identities, passkeys, sessions, email tokens, sign-in events | `catalog_state` (catalog id, `generation`) |
| `api_keys`, `key_catalogs`, `user_catalogs`, `sign_in_codes`, `sign_in_code_catalogs` | `documents`, `revisions` |
| OAuth clients and tokens | `changesets`, `changeset_entries` |
| `catalogs` (the registry), `stores`, placement | `maintenance_*` |
| `unlocks` (tallies already run in their own statement, `src/app.rs:424`) | `audit_entries` for catalog events |
| `audit_people`, `audit_credentials`, `audit_teams`, account `audit_entries`, `audit_access_counts` | |
| `feedback`, `server_settings`, `server_state`, `server_keys`, `cluster_stats`, `apalis.*` | |

The registry row (`catalogs`) keeps every column it has today except
`generation`, and gains `store` and `provisioned_at`. `generation` moves to `catalog.catalog_state`, since
every publish bumps it in the store transaction.

### The stores table

```sql
CREATE TABLE stores (
    name          text PRIMARY KEY CHECK (name ~ '^[a-z0-9-]+$'),
    -- 'account-database', or a host, port and database name.
    location_kind text NOT NULL CHECK (location_kind IN ('account-database', 'host')),
    host          text,
    port          integer,
    database      text,
    -- 'iam', 'secrets-manager' or 'env'; the reference names where the
    -- credential is, never the credential.
    credential_kind text CHECK (credential_kind IN ('iam', 'secrets-manager', 'env')),
    credential_ref  text,
    created_at    timestamptz NOT NULL,
    CHECK ((location_kind = 'account-database') =
           (host IS NULL AND port IS NULL AND database IS NULL AND credential_kind IS NULL))
);
```

Placement is a `default_store` server setting plus a nullable
`teams.store`.

### Routing a request

1. The request names catalogs. The server resolves each name through an
   in-memory cache of registry rows to (id, store). An account-database
   `NOTIFY` clears a row when its catalog is created, deleted or changed.
2. `app.catalog_db(&catalog)` returns the store's `CatalogDb`, opening the
   pool on first use from the `stores` row (and fetching a credential for
   `iam` or `secrets-manager`).
3. The existing per-catalog publish lock (`pg_advisory_xact_lock` on the
   catalog id) runs in the store transaction. Catalog ids are unique across
   stores, so its keys stay unique.

### Cache signalling

Postgres delivers `NOTIFY` within one database, so each replica keeps one
listening connection to the account database and one per store it has
opened. The existing response to a lost listening connection (drop the
cache it covered) applies per store.

### Audit views

The owner and admin audit views read the account database and every store
holding the viewer's catalogs, and merge by time. The paging cursor
becomes (time, store, id), with the account database as its own store
value, so a page continues exactly where the last one stopped.

### Migrations

Two folders, `migrations/accounts/` (today's files) and
`migrations/catalog/`. At startup the server migrates the account
database. A store is migrated the first time its pool opens, and the new
`okf-mcp migrate-stores` command migrates every store at once. The
existing per-database advisory lock keeps two replicas from migrating the
same database together.

`migrations/catalog/` starts with a baseline that creates the whole
`catalog` schema, for new stores.

## Migrating today's database

Three releases, each deployable alone:

1. **Types only.** `AccountDb` and `CatalogDb` both wrap the one pool, and
   every call site moves to the right one. No schema change and no change
   in behaviour. This is most of the code.
2. **Registry, stores and placement.** One account migration adds
   `stores` (seeded with `main`, location `account-database`), the
   `default_store` setting (`main`), `teams.store`, and `catalogs.store` and
   `provisioned_at` (existing catalogs: `main`, provisioned at their
   `created_at`). Routing now goes through the registry; everything still
   lands on `main`.
3. **The split.** One account migration moves the catalog tables into
   schema `catalog` with `ALTER TABLE ... SET SCHEMA` (metadata only),
   creates `catalog.catalog_state` from `catalogs.generation`, drops the
   foreign keys that cross the line, creates `catalog.audit_entries`, and
   moves the existing catalog-event entries into it. The server then
   **adopts** the moved tables: when `catalog` holds the tables but
   `catalog._sqlx_migrations` is empty, it records the baseline as applied
   (with the checksum embedded in the binary) instead of running it, once,
   under the migration lock.

Releases 2 and 3 are not reversible by migration; each deploy is preceded
by a database snapshot. Self-hosted installs take the same path on
upgrade.

## Privacy and security

- The stores table cannot hold a password (Decision 4). Credentials are
  fetched at pool open and never logged.
- A store's catalog content, history and catalog audit (paths, messages)
  stay on that store. Shared components still carry some of a store's
  data: the account database (names, emails, credential labels, catalog
  names), the job queue (ids only, Decision 14), replicas' in-memory
  caches, and logs and traces. Anyone describing a store as segregated
  should say which of these are shared.
- The compile-time split (Decision 7) is the main guard against a query
  reaching the wrong database.

## Testing

- **Two databases in the harness.** Catalog tests run with the store in
  the account database and with the store in a separate database.
- **Schema equality.** A database migrated from the pre-split schema and
  adopted has the same `catalog` schema (compared from a schema-only dump)
  as a freshly created store.
- **Isolation.** A catalog on store B writes no row to store A's `catalog`
  schema, and the reverse.
- **Placement.** A team pin wins over the default; changing the default
  changes new catalogs only.
- **Crash recovery.** Stopping between a create's steps, or a delete's,
  leaves state the job finishes or tidies, and a half-created catalog is
  never visible.
- **Unreachable store.** A multi-catalog read returns `unavailable` for the
  store's catalogs; a write to it fails with a clear error.
- **Audit merge.** The merged audit view pages correctly across the account
  database and two stores.
- **Jobs.** Per-store housekeeping runs once per store and slot across
  replicas, and one store failing does not hold up the others.
- **Compiler.** The type split is itself the routing test: the build fails
  if a query uses the wrong database.

## Docs

- `docs/operations.md`: stores, placement, `migrate-stores`, credential
  kinds, the connection-count arithmetic (stores x replicas x pool size).
- `docs/reference/tools.md`: the `unavailable` field on multi-catalog
  reads.
- Admin API reference: stores and placement endpoints.

## Relation to the revision storage epic (#954)

This epic does not depend on the revision storage epic (deltas, content
addressing, object storage), and goes first: its release 1 moves every
call site to `AccountDb` or `CatalogDb`, so the storage epic is written
against those types instead of being converted afterwards. The two touch
the same history and write code, so they run one after the other, not in
parallel.

The storage epic inherits three things from this design:

1. Its format migration is a `migrations/catalog/` migration that runs on
   every store, and stores migrate on first use, so the conversion must be
   resumable per store.
2. If content moves to object storage, each store needs its own bucket (or
   at least its own encryption key) to keep segregation; that is a new
   field on the `stores` row.
3. It defines the full-history export a catalog move will use.

## Later work this design leaves room for

- Moving a catalog between stores, after the revision storage epic.
- Per-store connection budgets, or a pooler in front of a cluster.
- Dedicated cells: a separate deployment with its own account database.

# mcpjam scenarios: a temporary catalog per scenario

## What

`tests/mcpjam` can give every scenario its own temporary catalog, created and
deleted by the run through the REST admin API (`POST` and `DELETE
/api/v1/catalogs`). Before this, every scenario shared one catalog, each in
its own folder.

- **`OKF_MCP_ISOLATION`:** `auto` (default), `catalog` or `folder`. `auto`
  tries catalog isolation and falls back to folder isolation when the key may
  not create catalogs (403). `catalog` refuses to run with such a key.
  `folder` keeps the previous behaviour and needs `OKF_MCP_CATALOG`.
- **Key:** catalog isolation needs an admin key that reaches every catalog
  (`--access admin --all`). It creates the catalogs, owned by the key's user
  (`GET /api/v1/me`), and writes in them over MCP. Local mode now makes that
  kind of key with the CLI, so local runs take the same path as staging.
- **Delete guard (`src/admin.ts`):** a catalog is deleted only if this run made
  its name (`mcpjam-<run>-<scenario>`) and created it. Anything else is refused
  before a request is sent. `scenarios/admin-guard.test.ts` tests this without
  a server. If seeding fails, the setup deletes what it created before
  rethrowing, because vitest runs no teardown for a setup that threw.
- **Harness:** `useScenario(scenario)` opens one MCP session per `describe`,
  bound to that scenario's catalog. `OkfSession` now carries its catalog.
  `Target` has `isolation` and a `catalogs` map by slug in place of `catalog`.
- **Concurrency suite:** its 8 sessions still share one catalog, now that
  scenario's own catalog.

## Why

Folder isolation left empty folder indexes and `log.md` lines in the shared
catalog after every run, and every scenario still shared the root
`index.md`. A catalog per scenario starts each one from a clean catalog and
leaves nothing behind (#759).

## Verification

- Local, catalog isolation: 16 catalogs; 27 passed plus 1 expected fail.
- Local, `OKF_MCP_ISOLATION=folder`: green.
- Remote mode against a separately started server, which also held two
  unrelated catalogs:
  - an admin `--all` key with `auto` chose catalog isolation, went green and
    deleted all 16 temporary catalogs;
  - a write key with `OKF_MCP_CATALOG` fell back to folder isolation and went
    green;
  - a write key with `OKF_MCP_ISOLATION=catalog` was refused with the key hint;
  - both unrelated catalogs were still there afterwards.
- `tsc --noEmit` is clean.

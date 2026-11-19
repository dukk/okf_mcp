# MCPJam swarm goals as a deterministic test suite

## What

`tests/mcpjam/` is a new Node/TypeScript package (vitest and `@mcpjam/sdk`
10.11.0). It replays the 15 goals of the MCPJam swarm as fixed MCP tool calls,
with no LLM inference, and adds a shared-catalog concurrency suite. `just
test-mcpjam` runs it.

- **Targets:** local by default. The setup creates a user, catalog and write
  key with the `okf-mcp` CLI on a throwaway Postgres (`scripts/test-pg.sh`) and
  serves them on a free loopback port. Remote when `OKF_MCP_URL`,
  `OKF_MCP_KEY` and `OKF_MCP_CATALOG` are set, e.g. staging.
- **Isolation:** each run writes under `/swarm-<time>-<random>/`, one
  subfolder per scenario, seeded in one publish before the tests. This
  includes the `/infra/ntp` concept that "Verify and stamp a fact" assumes. In
  remote mode the teardown deletes the run's concepts (`OKF_MCP_KEEP=1` keeps
  them). Folder `index.md` files remain, because the server never deletes them.
- **Concurrency suite:** 8 concurrent MCP sessions on one catalog. Covers
  parallel dependent calls, fan-out writes to one changeset, concurrent
  publishes into an existing and a brand-new folder, racing edits of one line
  (exactly one wins), and cross-session visibility after publish.

## Why

The 2026-11-19 swarm wave had 5 red runs out of 15, and none was a server bug.
The causes were a fixture the goal assumed but that didn't exist, a simulated
user that went off-script, two misgradings, and one run too long for the
judge's context. Each wave also costs inference. This suite checks the same
server contracts at no inference cost, so a red test means the server changed.

## Findings recorded in the tests

- `add_source` after `create_concept` in the same changeset works when the
  calls are sequenced. The swarm's NotFound came from parallel calls (#758).
- Two sessions creating the first concepts of one new folder both publish:
  index bookkeeping is replayed. The swarm's Quiz-session Conflict came from
  free `repair_indexes` and `edit_document` edits of the index, not from
  `create_concept`.
- Search can't find concepts by source id. This is a `test.fails` that goes
  red when #761 lands.

## Verification

- Local mode: 4 files, 25 passed plus 1 expected fail.
- Remote mode, against a separately started local server: two consecutive runs
  on one catalog, both green. After clean-up, only `log.md` and folder indexes
  remained.
- `tsc --noEmit` is clean.

Related: #758, #759, #760, #761.

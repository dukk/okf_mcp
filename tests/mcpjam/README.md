# MCPJam swarm scenarios, without inference

The MCPJam swarm for this server runs 15 goals: an LLM plays a persona, another
model drives the assistant, and a third model grades the result. That costs
inference on every run, and a red result can come from the simulated user or
the grader as easily as from the server.

This suite replays the same goals as fixed sequences of MCP tool calls through
the [MCPJam SDK](https://www.npmjs.com/package/@mcpjam/sdk) (`MCPClientManager`).
It asserts on what the server stores and answers. No model is involved, so a
red test is a server behaviour change.

| File | Swarm persona | Goals |
|---|---|---|
| `scenarios/careful-fact-curator.test.ts` | Careful Fact Curator | Create and source a fact; Review and publish a draft; Add context with sources; Organize facts into folders; Verify and stamp a fact |
| `scenarios/skeptical-recall-prober.test.ts` | Skeptical Recall Prober | Create and retrieve a stored fact; Feed scattered facts and audit retention; Test source attribution accuracy; Provoke and inspect edit conflicts; Catch hallucinated recall with a search audit |
| `scenarios/playful-memory-tester.test.ts` | Playful Memory Tester | Quiz the assistant on facts; Build a personal facts knowledge base; Test memory with surprise questions; Create indexed memory archive; Quick memory capture interview |
| `scenarios/concurrency.test.ts` | none | The shared-catalog races the 2026-11-19 wave hit, across 8 concurrent MCP sessions |

Each `describe` is named after its MCPJam goal, verbatim.

## Running

Local, on a throwaway Postgres (needs Docker and Node 22+):

    just test-mcpjam

Against a remote server, such as staging:

    export OKF_MCP_URL=https://<staging host>/mcp
    export OKF_MCP_KEY=...          # an API key with write access to the catalog
    export OKF_MCP_CATALOG=<scratch catalog>
    just test-mcpjam

Keep the key in your environment, never in a file.

| Variable | Meaning |
|---|---|
| `OKF_MCP_URL` | The MCP endpoint. Set: remote mode. Unset: local mode. |
| `OKF_MCP_KEY`, `OKF_MCP_CATALOG` | Required in remote mode. Use a scratch catalog: the suite writes to it. |
| `OKF_MCP_KEEP=1` | Remote mode: keep the run's concepts instead of deleting them afterwards. |
| `DATABASE_URL` / `TEST_PG_URL` | Local mode: the Postgres to use (`just test-mcpjam` provides one). |
| `OKF_MCP_BIN` | Local mode: the `okf-mcp` binary (default `target/debug/okf-mcp`). |
| `RUST_LOG` | Local mode: the server's log filter (default `warn`). |

## Isolation

Every run writes only under its own folder, `/swarm-<time>-<random>/`, with one
subfolder per scenario. The global setup seeds every subfolder in a single
publish before any test runs. So scenarios never share an index, goals that
assume an existing concept find it (`/…/verify-and-stamp/infra/ntp`), and two
runs against one staging catalog don't collide. Local mode also gets its own
user, catalog and key, created with the `okf-mcp` CLI.

In remote mode, the teardown deletes the run's concepts. The server never
deletes `index.md` files, so each run leaves its empty folder indexes and its
`log.md` lines behind. That is why remote mode should use a scratch catalog.

## Concurrency

`concurrency.test.ts` collides on purpose. Against a multi-replica staging, its
sessions may also reach different replicas:

- **Dependent calls in parallel** (`create_concept` with `add_source`, the swarm's
  false "add_source NotFound" finding). Either order must give a valid answer,
  and a sequential retry must succeed.
- **Fan-out:** 8 sessions write to one changeset at once, and no write is lost.
- **Concurrent publishes into one folder,** existing or brand new (the Quiz
  session's Conflict). Index bookkeeping is replayed, so every publish lands.
- **Racing edits of one line:** exactly one publish wins; the others get `Conflict`.
- **Cross-session visibility:** a publish is readable by every other session as
  soon as it returns.

## Known gaps

The test `search by source id finds the concepts citing it (#761)` is a
`test.fails`: it passes while the gap exists. When #761 lands it goes red;
then change it to a plain `test`.

# "Remember this" means saving to the catalog (issue #831)

Time: see the file name (UTC).

## Why

- In MCPJam swarm runs a model told "remember this: my dog's name is Miso" kept the
  fact in the conversation and declined to save it, because the instructions read as a
  shared team knowledge base. The goal failed with no tool call, so no tool-error metric
  showed it.

## What changed

- `INSTRUCTIONS` (`src/mcp/mod.rs`) and the tool reference's introduction
  (`src/mcp/reference.rs`, regenerating `docs/reference/tools.md`) gain one short
  statement: when the person asks you to remember something, save it to the catalog under
  a fitting folder, asking is their consent so do not ask again, and do not save what they
  did not ask you to keep.
- The `send_feedback` line, with its privacy caution about third-party forms, is unchanged.
- New test `the_instructions_tell_a_remember_request_to_save_to_the_catalog`.

## Not checked here

- The acceptance item about a later swarm wave (the "Feed scattered facts and audit
  retention" and "Quick memory capture interview" goals) can only be checked by running
  that wave after this ships.

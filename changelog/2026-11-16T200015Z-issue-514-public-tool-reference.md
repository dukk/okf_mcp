# Issue 514: link tool descriptions to a public tool reference

Part of epic 507 (connector directory review): the "Reference the target API docs in the
description" suggestion, addressed once for every tool.

## Added

- `docs/reference/tools.md`, the public tool reference: every tool (42), its title, kind
  (read-only, staged or destructive), description and parameters. A `## \`name\`` heading
  per tool makes `#name` a stable anchor on GitHub.
- `src/mcp/reference.rs` (test-only) renders that page from `OkfServer::tools().list_all()`.
  `the_checked_in_tool_reference_matches_the_tool_list` fails when the file differs; refresh
  it with `OKF_UPDATE_TOOL_REFERENCE=1 cargo test --lib mcp::reference`. A generated file with
  a drift test was chosen over a page the server serves: the page is public without the
  server being reachable, needs no route, auth exemption or deployment, and is reviewed in
  the PR that changes a tool.

## Changed

- `OkfServer::tools()` ends every tool's description with
  ` Reference: <TOOL_REFERENCE_URL>#<tool name>`. One place, so no description can forget it;
  about 100 characters per tool.
- README: opens as a description of Telekin and links the reference; the tool tables
  describe what `oldest`, `sprawl`, `links_report`, `lint`, `move_concept`,
  `split_concept`, `remove_source` and `repair_indexes` do instead of naming `okf.py`
  commands. The `okf.py` mentions left are about the Python CLI itself (its origin, the
  parity tests and the golden files, the JSON key order they compare).
- `src/investigator/contract_tools.json` regenerated: only description text changed.

## Notes

- The link points at `main`, so it resolves once this lands there.

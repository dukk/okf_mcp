# Issue 515: remove okf.py and contributor notes from tool-facing text

Part of epic 507 (connector directory review).

- Tool descriptions for `stamp`, `review`, `question`, `unquestion`, `source`, `log`,
  `index`, `oldest`, `sprawl`, `links_report`, `lint`, `move_concept` and `split_concept`
  no longer name `okf.py`.
- Doc comments that schemars publishes as schema `description`s (report row and key-order
  notes, `StampField`, `LogKind`, `Finding`, `WriteReport`, the `index_section` and
  `QuestionRef` fields) now describe the server's own behaviour. The contributor notes
  (`Copy`, `rename_all`, `#[default]`) moved to `//` comments beside them.
- The sprawl lint warning now says "the sprawl tool on <path>" instead of "okf.py sprawl
  <path>". The one matching line in `tests/golden/analysis.json` was edited by hand, and
  `tests/parity.rs` notes it.
- New test `no_tool_text_names_okf_py_or_carries_a_contributor_note` (in
  `src/mcp/annotations.rs`) walks `OkfServer::tools().list_all()`, serializes each tool and
  checks every `description` in it, including the input and output schemas.
- `src/investigator/contract_tools.json` was regenerated and did not change.

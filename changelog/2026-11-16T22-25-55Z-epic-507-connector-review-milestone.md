# Epic #507: Claude connector directory review (milestone close-out)

Epic #507 answers the Claude connector directory's automated review of the 42
tools: 5 "Add a type to this parameter" items to fix and one repeated suggestion,
"Reference the target API docs in the description". This entry closes the
milestone: what each pull request delivered, where the work differs from the
issues, how it was validated, and what is left. It is docs only.

## What the epic delivered

Every tool parameter now has a direct schema `type`, a test keeps it that way
(the allow-list of known offenders is empty), every tool description links to
a public reference page, and tool-facing text no longer names `okf.py`, a
contributor note or a CLI flag a client cannot use.

| Issue | PR | What it delivers |
|---|---|---|
| #508 | #517 | `every_tool_parameter_has_a_direct_type` and the shrinking `KNOWN_UNTYPED_PARAMETERS` allow-list (`src/mcp/annotations.rs`); it found a sixth offender, `log.kind` |
| #509 | #525 | `maintenance_dispose.disposition` is an inline string enum (`disposition_schema`) |
| #510 | #529 | `maintenance_submit.submission` is an inline object (`#[schemars(inline)]` on `Submission`) |
| #511 | #520 | `stamp.field` is an inline `oneOf` with `"type": "string"` |
| #512 | #521 | `unquestion.question` carries `"type": ["integer", "string"]` |
| #513 | #540 | `set_frontmatter.value` lists the seven JSON types, with typed `items` |
| #514 | #527 | `docs/reference/tools.md` (42 tools), a drift test, and a ` Reference: <url>#<tool>` suffix on every description |
| #515 | #522 | `okf.py` and contributor notes removed from tool descriptions and published schema text, with a guard test |
| #531 | #539 | `log.kind` carries `"type": ["string", "null"]` |
| #532 | #537 | `split_concept` and `index` notes name tool parameters, not `--flags`; a guard over the ops script's output |
| #538 | #541 | Two HTTP refusals no longer name `--all` or `key create --all` |

Main was merged into the milestone branch four times (#518, #526, #536 and
#542); those PRs carry no review and are not sub-issue PRs. The sub-issues were
closed by coordinator comments ("Merged into milestone/507-connector-review by
#N"), because `Closes #N` does not fire on a base that is not the default branch.

## Divergences and decisions

- **#512: no `minimum: 1`.** The issue allowed a lower bound on the integer
  form. It was left off `unquestion.question`: it would sit on the property next
  to the string form and make the schema inexact.
- **#515: how `tools/list` was checked.** A test serializes
  `OkfServer::tools().list_all()`; the list was not captured from a running
  server. Some internal comments next to the changed doc comments were reworded
  along the way. One golden line (the sprawl lint message in
  `tests/golden/analysis.json`) was edited by hand, and `tests/parity.rs` says to
  reapply it after regenerating from `okf.py`.
- **#510: nested `$ref`s stay.** The issue asked to check whether the nested
  types (Evidence, FindingInput, Assessment) should be inlined too. They were
  left as `$ref`s. The directory review's check and
  `every_tool_parameter_has_a_direct_type` both work at the parameter level;
  `submission` now has a direct `"type": "object"`; inlining the nested types
  would reshape every other use of them. If the directory review's re-run flags
  nested types, that is follow-up work.
- **#509: module move and `schema_with`.** The params struct moved into its own
  module for the dylint exemption. `schema_with` was used because
  `#[schemars(inline)]` is container-only and would reshape every use of
  `Disposition`.
- **The attribute pattern per issue.** #511: enum-level `inline` plus
  `extend("type" = "string")`. #512 and #531: a field-level
  `extend("type" = [...])` next to the `$ref`, so the values stay defined once.
  #513: the seven JSON types (string, number, integer, boolean, array, object,
  null) plus typed `items`. #508 chose per-field fixes over a generator-wide
  inline setting, which would not cover #512 or #513 and would reshape every
  tool's schema.
- **#514: a link on all 42 tools.** Every tool's description ends with
  ` Reference: <url>#<tool>`, about 100 characters each, for all 42 tools and
  not only the ~20 the review flagged. The links point at `blob/main`, so they
  resolve only after the milestone merges. The page renders four kinds of tool:
  read-only, staged, maintenance and destructive. A generated page with a drift
  test was chosen over a page the server serves.
- **#532: the guard's reach.** The CLI-flag guard (`ops_output_never_names_a_cli_flag`)
  covers only the ops script's output. The `index` missing-title refusal is
  pinned by its own unit test, not by the guard. The sweep left `--force`,
  `--replace` and `--actor`, which only the command line shows (or which the
  HTTP layer rewrites).
- **#538: the CLI message changed too.** The store message lost `(--all)` for CLI
  callers as well. The `--actor` messages in `src/store/keys.rs` are reachable
  from the CLI only: the callers are `src/cli.rs` and `src/cli/people.rs`, and
  HTTP uses the `*_by_id` functions. A broad guard over all HTTP refusals was
  considered and declined.
- **#521 (#512) merged under the 3-review cap.** Its last review was of
  4db5f16; the merged head 987fac8 only adds a milestone sync merge, and the
  PR's own delta is otherwise identical.
- **Process: freshness on an unprotected branch.** On the unprotected milestone
  branch GitHub reports a PR as CLEAN even when it is behind, so freshness was
  checked with `git merge-base --is-ancestor`.

## Validation

- Full gates on the final tree (f9949f7, identical to 8f44bce): fmt, lint,
  dylint and test green; 2322 passed, 0 failed.
- Narrow re-runs on 8f44bce: `mcp::` 170 passed; parity 8 passed, 7 ignored (the
  ignored ones need `okf.py`).
- There is no CI on pull requests or main: the only workflows run on `v*` tags
  and manual dispatch.

## Pending after the merge

- The deploy.
- A re-run of the Claude connector directory review; expect 0 to fix.
- The reference links resolving (they 404 until `docs/reference/tools.md` is on
  main).
- Ticking the decomposition checklist on #507 and closing it.

## Follow-ups

Filed:

- #533: flaky UI test `the_last_sign_in_method_stays_and_the_page_says_why`.
- #534: `watchdog.sh` false alarm.
- #500: load-sensitive tests. It also covers the
  `investigator_client_init_http_failure_classification` and
  `each_poll_tick_is_its_own_trace` flakes seen during this epic.

Found but not filed:

- The reference page renders multi-type and nested array types awkwardly.
- `log.kind` renders on the page as a link to its type, not as its direct type.

# Issue #508: test that every tool parameter schema has a type

## Added
- `every_tool_parameter_has_a_direct_type` (`src/mcp/annotations.rs`): walks every registered tool's input schema and fails when a top-level property, or the `items` of an array property, has no direct `type` keyword (a bare `$ref`, `anyOf` or `oneOf` without `type`, or a `true` or `{}` schema). This is the connector directory reviewer's check, run locally.
- `KNOWN_UNTYPED_PARAMETERS`: an explicit allow-list of the offenders. It only shrinks; each of #509 to #513 deletes its entry, and the test also fails when an entry is typed already, so stale entries cannot linger.
- `the_type_check_rejects_the_shapes_the_reviewer_rejects`: unit test of the checker on each rejected shape.

## Changed
- Nothing outside tests. `src/investigator/contract_tools.json` is unchanged because no schema changed.

## Notes
- Fix pattern: per-field (a `schemars` attribute on each field), recorded in the test's doc comment. A generator-wide inline setting would at best cover #509 to #511, not #512 or #513, and would reshape every tool's schema.
- The test found a sixth offender the review did not list: `log.kind` (`anyOf` of `$ref` and null). It is in the allow-list with a comment.

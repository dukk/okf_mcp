# Issue #509: give maintenance_dispose.disposition a schema type

## Changed
- `MaintenanceDisposeParams.disposition` (`src/mcp/maintenance/reports.rs`) carries a `schema_with` function (`disposition_schema`), so its schema is the enum inlined (`"type": "string"` with the four values) instead of a bare `$ref` to `Disposition`. The accepted values are unchanged: the serde side is untouched.
- Removed the `("maintenance_dispose", "disposition")` entry from `KNOWN_UNTYPED_PARAMETERS`.

## Added
- Tests: the inlined schema shape, and that all four dispositions still deserialize while an unknown spelling is refused.

## Notes
- Same pattern as `submission_outcome_schema` (#510 can reuse it for `Submission`'s `$ref`). The hand-written enum list could drift, so a test compares it with the enum's serde forms. `#[schemars(inline)]` was tried first but is container-only and would reshape every use of `Disposition`.
- `src/investigator/contract_tools.json` does not record this tool, so it is unchanged.

# Issue #510: maintenance_submit.submission gets a direct object type

The connector directory review flagged `maintenance_submit.submission` as a
bare `$ref` with no `type`. `Submission` now carries `#[schemars(inline)]`, so
the property is the struct's own schema: `"type": "object"` with the full
`properties`, `required` and `additionalProperties: false`, generated from the
struct so it cannot drift. `Submission` is used only by this tool's schema, so
no other output changes, and what the tool accepts is unchanged.

- Removed the `("maintenance_submit", "submission")` entry from
  `KNOWN_UNTYPED_PARAMETERS`.
- Refreshed `src/investigator/contract_tools.json`: only the `Submission`
  definition moved from `$defs` into the property.
- Added `submit_schema_tests` in `src/mcp/maintenance/work.rs`.

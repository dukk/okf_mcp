# Issue #513: set_frontmatter.value gets a direct type

The connector directory review flagged `set_frontmatter.value` as an any-value
schema with no `type`. `value` is a `serde_json::Value`, and the op accepts
every JSON kind: a string, number or boolean is written as a scalar, an array
or object as a flow or block value, and a null deletes the key (`type` cannot
be deleted). So the property now lists `string`, `number`, `integer`, `boolean`,
`array`, `object` and `null`, with a typed `items` for the array case. Nothing the tool
accepts changes, and a missing `value` is still a parameter error.

- Removed the `("set_frontmatter", "value")` entry from
  `KNOWN_UNTYPED_PARAMETERS`.
- Added `set_frontmatter_value_schema_tests` in `src/mcp/writes.rs`.
- Refreshed `docs/reference/tools.md`. `src/investigator/contract_tools.json`
  does not record `set_frontmatter`, so it is unchanged.

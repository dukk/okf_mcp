# Issue #511: give stamp.field a schema type

## Changed
- `StampField` (`src/okf/ops/mod.rs`) now carries `#[schemars(inline, extend("type" = "string"))]`. The `stamp` tool's `field` property is the inlined `oneOf` of the two choices with a direct `"type": "string"`, instead of a bare `$ref`. The connector directory reviewer's "Add a type to this parameter" finding no longer applies. What the tool accepts is unchanged: `generated` and `verified`, nothing else.
- Removed `("stamp", "field")` from `KNOWN_UNTYPED_PARAMETERS` (`src/mcp/annotations.rs`).

## Added
- `stamp_field_has_a_direct_string_type_and_accepts_what_it_did`: the schema has `type: string` and both choices, and deserialization still accepts the two spellings and rejects other cases, other words, empty and non-string input.

## Notes
- `src/investigator/contract_tools.json` records no input schemas, so it is unchanged.
- Description text is untouched (owned by #515).
- The same two attributes would fix `log.kind` only partly: it is an `Option` of the enum, so the property is an `anyOf` with null; it needs the nullable form (`type: ["string", "null"]`) on the field.

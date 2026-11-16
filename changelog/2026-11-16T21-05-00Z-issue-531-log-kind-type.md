# Issue 531: give `log.kind` a direct schema type

The `log` tool's `kind` property was published as `anyOf` of a `$ref` and
null, with no `type`, which the connector directory's check rejects.

- `LogParams.kind` now carries `#[schemars(extend("type" = ["string", "null"]))]`,
  as `UnquestionParams.question` does (#512). The four kinds still come from
  `LogKind`'s own schema through the `$ref`, so they are defined once.
- What the tool accepts is unchanged: the four capitalized kinds, null, or no
  `kind` (meaning Update).
- Removed `("log", "kind")` from `KNOWN_UNTYPED_PARAMETERS`.
- Added tests: deserialization of each kind, null, omitted and an unknown kind;
  the schema's `type` and the four kinds.
- `docs/reference/tools.md` and `src/investigator/contract_tools.json` needed no change (the drift test passes).

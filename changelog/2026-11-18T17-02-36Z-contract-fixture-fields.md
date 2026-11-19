# Investigator contract test compares only what the runtime compares

## Added
- `same_contract` in `src/investigator/contract.rs`: the one comparison of a live tool against its checked-in contract (name, normalized input schema, normalized output schema).
- Test `investigator_contract_ignores_titles_and_hints_but_not_schemas`: for each of the nine tools, changing `title`, `description` or `annotations` is accepted, and adding a property to the input or output schema is rejected.

## Changed
- `validate_tools` calls `same_contract`, and `investigator_contract_rejects_missing_or_changed_tools` calls it too, instead of comparing whole serialized tools with `contract_tools.json`. A title or hint change no longer needs the capture test to be re-run.
- `contract_tools.json` is unchanged: it still holds the whole captured tools, and the capture test is untouched. The fields the check ignores are informational there.

## Notes
- The test is still strict on name and schemas. It is now also lenient about descriptions, as the runtime check already was.

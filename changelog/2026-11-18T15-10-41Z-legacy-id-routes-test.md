# Keys API: legacy-id test covers flag, grant and ungrant routes (#237)

## Added
- `a_legacy_id_reaches_only_its_own_key_on_flag_grant_and_ungrant` in
  `src/http/api/keys.rs`: a table over `PATCH /keys/{id}`,
  `PUT /keys/{id}/catalogs/{catalog}` and `DELETE /keys/{id}/catalogs/{catalog}`,
  each sent to a legacy id while a live key of the same name exists. It asserts
  the legacy key changed and the same-name replacement is byte-for-byte the
  same in the key list. `POST /keys/{id}/revoke` was already covered.

## Changed
- Nothing in production code.

## Notes
- Revert-checked: pointing the grant handler, then the report-flag handler, at
  the name-based store functions made the new test fail; both restored.
- The legacy row from `insert_legacy_test_key` is an every-catalog key, which
  grants refuse, so the test narrows it to `main` first.

# Issue #642: store readers return an error for an unrecognised stored value

## Added

- `store::decode_column`: turns a failed `parse` of a stored column into
  `OkfError::Database` (a `sqlx::Error::ColumnDecode` naming `table.column`).
  The error names the column only, never the stored text.
- Test helpers `store::drop_check_constraints` and `store::assert_decode_error`.
- One corrupt-row test per reader family: `ceiling` (member access and
  entitlement level, including a rolled-back `change_member_access`),
  `changesets`, `sign_in_events`, `teams`, `unlocks`, `keys`, `oauth::codes`,
  `oauth::clients`.

## Changed

- Sixteen `X::parse(..).expect("the schema checks ...")` sites in
  `src/store` now return an error instead of panicking (the issue listed
  fourteen; `ceiling` has two more than the issue listed).
- `From<RawChangeset>`, `From<KeyRow>`, `From<CredentialRow>`, `From<CodeRow>`
  and `From<ClientRow>` became `TryFrom` with `Error = OkfError`;
  `UnlockRow::into_record` and `teams::assemble` return `Result`.
- `tokens::revoke` uses the shared helper. Its error no longer carries the
  stored kind text (#641 put it in the message).

## Notes

- Write paths: `ceiling::change_member_access` and `oauth::clients::insert_dcr`
  convert the row before `commit`, so the error drops the transaction and
  rolls back. Readers called inside a caller's transaction
  (`ceiling::read_catalog_locked`, `teams::current_role`, `changesets::lock`,
  and `changesets::open`, an `INSERT ... RETURNING` whose state is the literal
  `open`) propagate with `?` before that transaction commits.
- `OkfError::Database` is internal: clients see only the kind, never the text.

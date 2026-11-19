# M18 Task 3: audit rows are created with their entity

Issue #681, milestone 18 (bounded memory), spec Decision 20.

## Added

- `migrations/0023_audit_backfill.sql`. It creates the missing `audit_people` row for every user, the missing `audit_teams` row for every team, and the missing `audit_credentials` row for every live key and grant. The credential rows carry the owner's person and, as their label, the credential's `actor`. These are the rows the SQL resolvers would make. The migration inserts only, with `WHERE NOT EXISTS` and `ON CONFLICT DO NOTHING`, so a rerun changes nothing and no existing row is locked or refreshed.
- `audit::credential_created(conn, credential_id, label)`. It creates a new key's or grant's audit row, with its label, in the transaction that inserts the credential.
- Tests:
  - `store::gates::tests::every_new_user_key_grant_and_team_has_its_audit_row`
  - `store::gates::tests::gates_load_writes_nothing`
  - `db::tests::the_backfill_creates_missing_rows_and_is_idempotent`
  - new audit-row assertions in `codes::tests::redeem_turns_the_code_into_an_oauth_grant_once` and `grants::tests::a_self_issued_grant_records_its_user_levels_and_client`
  - the test helper `audit::testing::credential_row`

## Changed

- `keys::create_provisioner`, `keys::create_with_id` and `keys::create_self_issued` (through `record_created`), `oauth::codes::redeem` and `oauth::grants::create_self_issued` now create the credential's labelled audit row before recording the entry.
- `gates::load` writes nothing. `ensure_audit_rows` is removed, so a key-cache reload no longer issues an `INSERT` per missing row, and per-request loading (Task 11) can read the audit ids together with each entity.

## Notes

- `users::create`, `teams::create` and `catalogs::set_owner` are unchanged. Since `0014_audit_row_locks.sql` they have made their rows in their own transactions.
- Lock order is unchanged. `audit_credential` takes the owner `FOR KEY SHARE` through `audit_person`, and every caller already holds that user's lock from `audit::lock_people` (or `lock_credential_people` in `redeem`). The row it inserts is new and invisible to other transactions, so it waits on no audit row.
- The plan's Task 3 is corrected in this PR, with a dated note. The note covers the missing Rust helpers it named, the create paths that needed no change, and where the grant tests live.
- A key or grant inserted with plain SQL after this migration has no credential id until it acts. Only tests do that.

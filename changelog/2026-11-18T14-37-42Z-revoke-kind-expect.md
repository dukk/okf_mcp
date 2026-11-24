## Added
- A test (`revoke_answers_an_error_and_revokes_nothing_for_a_corrupt_kind`) that a row with an unparseable `oauth_tokens.kind` makes `tokens::revoke` return `OkfError::Database` and leaves the row unrevoked.

## Changed
- `store::oauth::tokens::revoke` no longer panics on an unparseable stored kind (issue #266). It runs in a transaction, maps the bad kind to `OkfError::Database` (a `sqlx::Error::ColumnDecode`, the error the driver gives for an undecodable column), and rolls back, so a bad row is never partly revoked. `POST /oauth/revoke` answers it as `server_error` with HTTP 500 and no detail, like any other store failure.

## Notes
- Other store readers still `.expect()` a parsed column (`ceiling.rs`, `changesets.rs`, `teams.rs`, `unlocks.rs`, `keys.rs`, `oauth/clients.rs`, `oauth/codes.rs`, `sign_in_events.rs`); they are not in `tokens.rs` and are left for follow-up issues.

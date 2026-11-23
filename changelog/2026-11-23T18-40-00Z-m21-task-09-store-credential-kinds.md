# M21 Task 9: credential kinds `secrets-manager` and `iam`

## Added
- `src/stores/credentials.rs`: a store's database login is found by a `CredentialSource`, with three implementations: `env` (a variable holds the password), `secrets-manager` (the reference is a secret ARN; the secret is JSON with `username` and `password`) and `iam` (an RDS IAM token, signed with `aws-sigv4`, valid 15 minutes). The AWS clients sit behind two small traits, `SecretReader` and `TokenSigner`, so tests mock them; the real ones load the same `aws-config` chain the SES mailer uses, once, on first use.
- A refused login (SQLSTATE class 28) while a store opens is fetched again and tried once more (not for `env`, whose value cannot change). After the open, one task per pool with a changing credential replaces the pool's connect options through `Pool::set_connect_options` (sqlx-core 0.9, `src/pool/mod.rs:555`): every 10 minutes for `iam`, and for `iam` and `secrets-manager` when a refused login is seen on that pool (at most one refresh per 10 seconds). The refusal is seen where every driver error passes: `CatalogDb::classify` and `OkfError::from(sqlx::Error)`, so a plain `executor()` read that fails after a rotation also triggers it. A token is signed for the region in an RDS host name, falling back to the server's region for a host name that does not say. The task belongs to the registry entry, so `Stores::forget` and `forget_all` end it.
- Dependencies: `aws-sdk-secretsmanager`, and `aws-sigv4`, `aws-smithy-runtime-api`, `http` (all but the first already in the tree), with `aws-credential-types` moved from dev to normal dependencies.
- Tests (review round adds `a_plain_read_refused_after_a_rotation_refreshes_the_login_and_the_next_read_works` and `a_token_is_signed_for_the_databases_region_not_the_servers`): `secrets_manager_credential_is_read_from_the_secret_json`, `iam_replacement_connection_after_the_first_token_expires_uses_a_fresh_token`, `iam_auth_failure_refreshes_once_and_retries`, `a_credential_never_appears_in_logs_or_errors`, and beside them: a secret that is rotated, a login that stays refused (two tries, no more; one for `env`), a refused login on an open pool, forgetting a store ends its task (one and all), the keeper's timer and gap on a paused clock, and the token's shape.

## Changed
- Credential resolution moved out of `registry.rs`. `Stores::with_providers` is the injection point for tests; `with_environment` remains as a test helper. The registry cache holds an entry (`OpenStore`) with the handle and the credential task.
- `CatalogDb::classify` turns a refused login into `StoreUnavailable` (the reply names nothing), where it was a `Database` error.
- `Stores::forget` settles the cache's bookkeeping before it returns.

## Decisions
- No `user` column and no migration, so Task 11 stays 0041 and Task 12 stays 0042. For `env` the user is the account pool's; for `secrets-manager` it is the secret's `username`; for `iam`, `credential_ref` is the user.
- IAM tokens are signed with `aws-sigv4` (a presigned `Action=connect` URL for `rds-db`) and not `aws-sdk-rds`, a large crate for one signing call.
- A request refused a login still fails; the next one succeeds. Retrying inside the pool would need a pool wrapper, and `set_connect_options` makes that unnecessary.

## Notes
- No credential, token or ARN reaches a log or a reply: `Credential` prints as `[redacted]`, a `CredentialError` holds only fixed text, a variable name or an AWS error code, and an AWS error's message (which can name the ARN) is dropped.

# M16 Task 8: OAuth pending authorizations in shared state (issue #561)

## Added
- `OAuth::pending` is an `Ephemeral<PendingAuthorization>` (`Keyspace::OauthPending`): in memory without `[redis]`, as before, and in Redis under `{<key_prefix>}oauth:pending:<hashed(pending id)>` with it, so a consent started on one replica can be continued, approved or denied on any (spec Decisions 5 and 7). `MAX_PENDING` stays in memory only (spec Decision 8).
- Serde on what a pending authorization holds: `PendingAuthorization`, `AcceptedCode` and `UserConsent` (their hand-written `Debug` kept), `ClientRecord` (chrono's `serde` feature, added in `Cargo.toml`), and `Access` and `ClientKind`, which serialize as their stored `as_str` spellings.
- `consent_change(form, fresh)` builds the consent POST's update closure: CSRF, the summary token and the signed-in nonce in one atomic update. It also refuses a request whose wrong codes are spent.
- `ending(oauth, id, brand, answer)` drops a pending authorization and answers `answer`, or the busy page when Redis fails.
- `oauth::testing::approval_form(test, query)`: the consent up to Approve, returning the unsent Approve form.
- Tests, each on standalone Redis and on a one-node cluster (`authorize::tests::replicas`): consent started on A, the code step and Approve on B, the code exchanged on A; a signed-in page's Approve posted to A and B at once approves once (4 rounds, one grant each); wrong codes alternated between A and B are counted together and end the request on both; an expired pending authorization (`PEXPIRE` on the hashed key; no key named by the raw id) is refused on both; a forced lost race reruns the consent closure and still spends the nonce; a Redis outage answers the busy page (503) at the GET and at a POST. Unit tests: the serde spellings and round trips.

## Changed
- The `authorize.rs` call sites await the store. Every closure is idempotent on a fresh copy: the consent nonce is cloned, not taken, and the accepted code is cloned, not moved (`lessons.md`).
- The consent page's `server_error` answers the busy page (503) for `OkfError::Busy` (a Redis failure or a database deadlock); other errors stay 500. Task 7 flagged this on #561.
- A Redis failure fails closed everywhere in the flow: the GET's insert, every update, and every remove (Deny, too many codes, a refused signed-in consent, and `finish` before it mints a code) answer the busy page, never a page saying the request ended.
- Task 7's `a_redis_outage_fails_closed` walks the consent up to Approve before stopping Redis, since the pending authorization now needs Redis too.

## Notes
- No per-actor `Index` was added: pending authorizations have no per-actor cap to move (the plan's Task 8 names none).
- The memory path is unchanged; its tests are unchanged.
- Revert-checked: `fresh.take()` in `consent_change` fails the forced-race test (the rerun refuses); not replacing the nonce in `claim_consent` gives two 200s across replicas; dropping the `Busy` branch of `server_error` answers 500 in the outage test.

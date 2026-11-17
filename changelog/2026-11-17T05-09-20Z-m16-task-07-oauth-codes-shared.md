# M16 Task 7: OAuth authorization codes in shared state (issue #560)

## Added
- `OAuth::codes` is an `Ephemeral<AuthorizationCode>` (`Keyspace::OauthCode`): in memory without `[redis]`, as before, and in Redis under `{<key_prefix>}oauth:code:<hashed(code_key(code))>` with it, so a code approved on one replica can be exchanged on any (spec Decision 5).
- `AuthorizationCode` derives `Serialize` and `Deserialize`. It holds bindings and flags only, never the code.
- `vouches_unreplayed` in `token.rs`: the post-commit replay check keeps the tokens only for a stored code whose `replayed` is unset. A code gone from the store, or a Redis error, revokes the family (spec Decision 16).
- Test helpers in `oauth::testing`: `oauth_replica_pair_on(redis)`, `send_on(app, ..)` (any app's router) and `approve(test, query)` (the consent flow up to Approve, its answer unread).
- Tests, each on standalone Redis and on a one-node cluster: a code minted on A is exchanged on B once (which also covers the exchange's grant catch-up) and a second exchange on A is the replay that revokes the family on both; two replicas racing one code, over 4 rounds, give exactly one replay and no usable token; an expired code (`PEXPIRE`) is refused on either replica; a Redis outage answers 503 at the token endpoint and the busy page at Approve, and mints nothing. Unit tests: the replay predicate, and the 503 mapping.

## Changed
- The exchange marks the code used with `Ephemeral::update`, a compare-and-set in Redis: only one write of `used` from `false` lands, and the loser reruns the closure on the stored copy and is the replay. The closure reads and changes only the stored value, so a rerun answers the same.
- `OAuthError::status`: `Server(OkfError::Busy { .. })` answers 503; every other server error stays 500. The body is unchanged (`server_error`, "internal error").
- The code insert on Approve awaits `Ephemeral::insert`; a full map or a Redis error both answer the existing 503 busy page.
- Task 6's `an_exchange_on_the_other_replica_catches_up_on_its_grant` (a code copied by hand into B's map) is replaced by the shared-code test above.

## Notes
- Codes use `update`, not `take`: `GETDEL` would drop the used code and with it the replay detection.
- The memory path is unchanged; its tests gained only `.await` and a `Result` to unwrap.
- Revert-checked: a plain write instead of the compare-and-set gives two 200s in the race; dropping the replay's `revoke_family` leaves the first token working; `app.keys().find_grant` instead of `access::find_grant` answers "the grant was revoked" on B; letting a Redis error vouch for the code fails the predicate's test.
- A code that expires in Redis between the exchange's first update and its post-commit check (a 60 s code exchanged in its last milliseconds) is read as gone and revoked: it fails closed, where the memory map, which reads as of the exchange's start, would keep it.

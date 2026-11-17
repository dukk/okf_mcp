# M16 Task 6: access-token revocation and grant catch-up across replicas (issue #559)

## Added
- A Redis revocation counter, `{<key_prefix>}oauth:access-revocations` (spec Decision 15). `AccessTokens::forget` bumps it after the revocation commits; with Redis on, every `verify` reads it first, and a value this instance has not seen clears its cache and bumps the cache generation, so no lookup in flight re-caches.
- Both counter scripts (`READ_REVOCATIONS`, `BUMP_REVOCATIONS`) re-create a missing counter inside the script from a random seed below 2^62 (`getrandom::u64() >> 2`), so a lost counter never repeats a value an instance has seen. Failures count `okf.redis.errors{operation="counter"}`.
- With Redis on, a cached entry is trusted for 60 s at most (`SHARED_MAX_AGE`, a monotonic `Instant` per entry), the backstop for a crash between a commit and its bump.
- `oauth::access::find_grant(app, grant_id)`: the key cache's grant, after one `App::catch_up_keys` when it is missing (spec Decision 14). `verify` and the code exchange use it.
- Test helpers in `oauth::testing`: `authorization_code` and `authorization_code_for` (moved from `token.rs`'s tests), `oauth_replica_pair` and `exchange_for_tokens`.
- Tests: a token revoked on A is refused on B at once; a lost counter at 1 still clears B's cache; a reader that finds the counter missing clears its cache; a Redis failure skips the cache and answers from Postgres (a revoked cached copy refused, a live token admitted, nothing cached); the 60 s backstop; a token minted on A accepted on B before any announcement; a refresh on B catches up on A's grant; the counter scripts in standalone and cluster mode; the seed's range.

## Changed
- `AccessTokens::new(redis: Option<Redis>)`, `OAuth::new(config, redis: Option<&Redis>)`; `App::load_with_redis` passes its handle.
- `AccessTokens::forget` is `async`; its callers in `token.rs` await it.
- The refresh path: a live refresh row whose grant the key cache lacks rolls the transaction back, catches up, and runs the refresh again once, so the catch-up never waits on the poll pass while holding the token rows' locks.

## Notes
- Without `[redis]` nothing changes: no counter, no age limit, and `catch_up_keys` is a no-op.
- `forget` does not record the counter value its own bump returned as seen: another instance's bump can fall between, and recording a value this instance did not read would hide that revocation.
- A Redis error on `verify` neither reads nor fills the cache; the database answers.
- The exchange's own catch-up is tested in Task 7, once a code can cross instances.

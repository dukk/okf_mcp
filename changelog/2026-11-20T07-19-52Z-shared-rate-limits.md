## Added
- `shared::limits`: rate limits shared by every replica (issue #357). With `[redis]` a limiter's token buckets live in Redis under `{<prefix>}ratelimit:<limiter>:<hashed id>`, taken by one Lua script that refills, takes and renews a bucket atomically on Redis's own clock, with a TTL of the time an empty bucket takes to refill. `KeyedLimit` wraps `http::limits::KeyedLimiter` (the per-address and per-email sign-in limits and the OIDC link allowance), `GlobalLimit` wraps `oauth::ratelimit::TokenBucket` (OAuth registration, token and consent).
- `okf_redis_errors_total{operation="limit"}`: a failed shared rate-limit call.

## Changed
- `SignInLimits::new`, `OAuth::new` and `OidcProviders::new` build their limiters on the app's Redis when `[redis]` is set; without it each limiter is the in-memory one, unchanged. The budgets, windows and refusals (429 pages, `slow_down`, the JSON 429) are unchanged.
- `try_take` on the sign-in, OAuth and link limiters, and `OidcProviders::may_start_link`, are now async.
- `docs/operations.md`: rate-limit buckets are listed in "What Redis holds", "When Redis fails" and "Sizing"; the per-replica caveats about rate limits are gone. The M12 plan (Decision 8), the user login spec and the multi-replica spec carry `Corrected (issue #357)` notes.

## Notes
- A Redis failure fails open to the replica's own in-memory bucket, which each limiter keeps for that purpose: the same budget, counted per replica as before #357, never unlimited. Failing closed would refuse every password sign-in, reset request and OAuth token refresh during a Redis outage, though they need only Postgres; the durable account lockout still applies. Each failure is counted (`limit`) and logged at `warn`.
- The optional in-memory first check from the issue is not used: it would refuse requests the shared bucket allows (a replica's own bucket also pays for requests the shared one refused), so it would make limits stricter than configured.
- Passkey ceremonies and OIDC flows were already shared through Redis by milestone 16 (`shared::ephemeral`, #565). `POST /api/v1/me/passkeys/confirm/start` (#355) is not limited per address: it needs a console session and its CSRF header, each session holds one ceremony at a time, and a passkey assertion cannot be guessed; the password confirmation of `DELETE /me` counts toward the durable lockout.

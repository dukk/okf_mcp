# M16 Task 10: passkey ceremonies in shared state (issue #563)

With `[redis]`, a passkey registration or sign-in begun on one replica
finishes on another, and a ceremony is taken once across all of them. Without
`[redis]` the ceremonies stay in this process's memory, as before (spec
Decisions 3-8 and 16).

## Added

- Tests in `src/http/passkeys.rs`, each two-replica body run on a standalone
  Redis and on a one-node Valkey cluster (`tests::standalone`,
  `tests::cluster`), over two `App`s with `poll_interval = 1h`:
  - a setup-link registration started on A and finished on B, which then
    signs in on B;
  - a sign-in started on A and finished on B;
  - a ceremony finished by both replicas at once (one 200, one 410), then
    replayed (410);
  - a session registration finishes only in the session that started it: a
    finish with another session's cookie is 403 and stores nothing, that
    session's own ceremony still finishes, and the restarted registration
    keeps only its own session (also run on one memory app);
  - no raw console session id, and no ceremony cookie, in any Redis key or
    value; the ceremony keeps the session's hash, which keys the index;
  - one session's registrations on A and then B hold one ceremony and one
    index key, and the replaced ceremony no longer finishes;
  - a Redis outage answers 503 to a finish and a start, with no fallback to
    memory;
  - no raw setup-link token in any Redis key or value, and the stored hash
    still finishes on the other replica;
  - a setup finish presenting another token (the user's newer link, or
    another user's) is 410, stores nothing and leaves that link usable (also
    run on one memory app).
- Test support: `mcp::testing::replica_pair_on` (two replicas over a Redis
  the test started, so a cluster too) and `Browser::send_to` (a request to a
  given replica). Task 9 (PR #576) added the same two; one copy of each was
  kept in the merge. The session hash is Task 9's `oidc::session_hash`.

## Changed

- `Cargo.toml`: `webauthn-rs` gains `danger-allow-state-serialisation`. The
  state is stored in Redis values only, server-side; it never reaches the
  client, a log line or a span.
- `src/http/passkeys.rs`:
  - `Passkeys::ceremonies` is an `Ephemeral<Ceremony>`
    (`Keyspace::PasskeyCeremony`) and `by_session` an `Index`
    (`IndexSpace::PasskeySession`); `Passkeys::new(config, redis)`.
  - `Ceremony`, `Registration`, `Pending` and `RegisterVia` derive serde
    (still no `Debug`).
  - `RegisterVia::Session` stores `session_hash`, not the raw session id. The
    finish refuses (403) unless the finishing request's own session hashes to
    it (constant time), and ends the other sessions by that request's own id.
  - `RegisterVia::Setup` stores `token_hash` (`keys::hash`, as Postgres
    stores the token), not the emailed token. `POST /auth/passkey/setup/finish`
    takes `{"token", "credential"}` and refuses (410) unless the presented
    token hashes to it (constant time), then redeems the presented token.
  - `remember` and `take_ceremony` are async and fallible; `take` is `GETDEL`
    in Redis. A Redis failure answers 503.
- `src/app.rs`: `Passkeys::new(&config, redis.as_ref())`.
- `src/http/ui/js/signin/passkey.js`: the setup page's finish sends the token
  again with the credential.
- Spec Decision 7 and the plan's Task 10 gain dated `Corrected (issue #563)`
  notes for the setup token.

## Notes

- The memory tests are unchanged apart from `.await` and the setup finish's
  new request body: every existing test that posts to
  `/auth/passkey/setup/finish` now wraps the credential in
  `{ "token", "credential" }` (through the `register_by_setup` helper, and
  directly in the cross-kind, hostile `return_to` and cross-site tests).
- Reading Redis gives no one a setup link or a console session: the
  ceremony keeps only their hashes (the maintainer, on review of PR #577).
- Revert-checked: dropping the token-hash check fails the other-token test
  (memory, standalone, cluster); a raw token in the stored ceremony fails
  `no_raw_setup_token_reaches_redis` in both modes; the old setup-page body
  fails the ui_e2e setup journey.
- Plan corrections are in Task 10's "Corrected (issue #563)" note.

## Added
- `store::server_keys` and migration `0028_server_keys.sql`: server-side keys, 32 random bytes per purpose, created on first use (`INSERT ... ON CONFLICT DO NOTHING`, then a fresh read, so replicas starting together agree) and kept in Postgres, so every replica reads the same key and it survives restarts. The first purpose is `audit_cursor`.
- `http::api::audit::cursor`: a team owner's audit cursor, the next entry id and the owner's `audit_teams` id sealed with ChaCha20-Poly1305 (ring) under the `audit_cursor` key, with a fresh random nonce and a fixed context string as associated data (issue #430).
- The `ring` crate as a direct dependency (already in the lock through rustls and sqlx).

## Changed
- The owner view of the audit log no longer carries any value of the global `audit_entries.id` sequence (issue #430). An owner's entries on `GET /api/v1/audit` and `GET /api/v1/audit/export` have no `id` key; an owner's `next_after` is the sealed cursor, a 59-character string, and is the only `after` an owner's request accepts. A raw id, an altered or truncated cursor, a cursor sealed under a rotated key, or one issued for another team the user owns is 400 `InvalidPath` with one message for every reason, and returns no entries.
- `AuditRecord.id` is `Option<i64>` (`None` for an owner, left out of the JSON), and the new unserialized `AuditRecord.cursor` holds the id for the server's own paging (`export_lines`, `okf-mcp audit list`).
- `GET /api/v1/audit`'s `after` is read as text and parsed once the viewer is known. For admins and provisioners it is still the raw integer id and `next_after` is still an integer; a non-integer `after` (an owner's cursor included) is now a JSON 400 `InvalidPath` rather than the query extractor's plain-text 400, and an empty `after=` is no cursor, like every other empty filter.
- `docs/operations.md` ("Audit API") describes the owner cursor, the key and its rotation; the operator API spec carries a `Corrected (issue #430)` note.

## Notes
- Encryption, not a MAC: a MAC alone would leave the id inside the token readable. A random nonce makes two cursors for the same place unrelated, so cursors cannot be compared or ordered either. The sealed team binds a cursor to the team it was issued for.
- No existing server secret fits: sessions, CSRF tokens and API keys are random per-credential values, not a server key, and the configuration holds none. A key in Postgres needs no new configuration and is shared by every replica. Nothing caches it, so deleting the row rotates it on every replica at once; a cursor sealed under the old key is then refused with 400, and the console's audit page shows that refusal on Load more until the owner reloads the first page.
- The console's audit page needed no change: it passes `next_after` back as `?after=` as an opaque value and never reads an entry's `id`.

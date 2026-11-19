# Idle OAuth grants are revoked, and every grant shows its last activity

## Added
- A grant's last activity (when its newest token was issued: the first token exchange and every refresh) shows as `last_activity T` at the end of `okf-mcp key list` and `okf-mcp oauth grant list` rows, as `last_activity` in the `/api/v1/keys` and `/api/v1/me/grants` answers, and as Last active on the console's API keys page and My grants (column and drawer). Issue #269.
- The poll loop revokes, at most hourly, every OAuth grant whose last activity (or creation, with no token on record) is older than the new `oauth.idle_grant_revoke_after` (default `"30d"`) and that holds no live token. The revocation is an operator revoke's (`store::keys::revoke_idle_grants` shares `revoke_by_id`'s locking and recording): `oauth.grant_revoked` with the actor `system:idle-grant-revocation` and details `{"reason": "idle", "last_activity": ...}`. Static keys are never touched.
- Migration `0021_oauth_tokens_grant_activity.sql`: an index on `oauth_tokens (grant_id, created_at)`, replacing `oauth_tokens_grant_id`, so the newest token per grant is one index probe.

## Changed
- `KeyRecord` and the API's `KeyView` carry `last_activity` (`None`/`null` for a key, or a grant with no token on record).
- `docs/operations.md` documents the setting, the new column and the "Idle grants" behaviour.

## Notes
- The issue's decision comment says to revoke tokens with no traffic for 30 days; this revokes the grant (and with it its tokens), since revoking only the tokens would leave the dead grant row listed, which is what the issue is about.
- Access-token use is not recorded per request, so token issue time is the activity signal; a working client refreshes at least every `oauth.access_token_ttl`.
- A grant that still holds a live token is never revoked, so a `refresh_token_ttl` longer than the idle period cannot cut off a client that could still refresh; with the defaults (both 30 days) the two conditions coincide.
- Several replicas may run the pass at once: each grant is re-checked under its row lock, so it is revoked and audited once.

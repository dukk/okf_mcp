-- A grant's last token activity is its newest token's `created_at`: the
-- key listing shows it (`store::keys::list`, `oauth grant list`, the
-- console), and the poll loop revokes grants idle past
-- `oauth.idle_grant_revoke_after` (`store::keys::revoke_idle_grants`). Both
-- ask `max(created_at)` per grant. Ordered by (grant_id, created_at), the
-- answer is the last entry of the grant's run, read without scanning the
-- grant's tokens. The index leads with `grant_id`, so it also serves
-- everything `oauth_tokens_grant_id` did (the cascade from `api_keys`), which
-- it replaces.
--
-- A plain CREATE INDEX, inside the migration's transaction, as 0018 and
-- 0020: it blocks writes to `oauth_tokens` while it builds, which on an
-- existing server is the time to read the table once at startup.
CREATE INDEX oauth_tokens_grant_created ON oauth_tokens (grant_id, created_at);
DROP INDEX oauth_tokens_grant_id;

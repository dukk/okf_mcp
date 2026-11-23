-- no-transaction
-- The catalogs a team's members reach through the team, by name in byte
-- order (issue #736): `GET /me/ceiling`, `GET /me` and the OAuth consent
-- page read a user's ceiling a page at a time, filtered by a name prefix
-- (`store::ceiling::page`). A prefix is a range in byte order (`COLLATE
-- "C"`) whatever the database's own collation, so the read is one range
-- scan per owner team that stops after a page. `catalogs_member_reach`
-- (migration 0025) keeps the listing order of the other pages. Built
-- CONCURRENTLY, one statement in a non-transactional migration, for the
-- reasons `0025_catalogs_member_reach.sql` gives.
CREATE INDEX CONCURRENTLY IF NOT EXISTS catalogs_member_reach_bytes
    ON catalogs (owner_team_id, name COLLATE "C") WHERE member_access <> 'none';

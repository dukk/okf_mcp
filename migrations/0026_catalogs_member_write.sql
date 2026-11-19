-- no-transaction
-- The catalogs a team's members may write through the team (issue #728):
-- `store::ceiling::highest` asks whether any catalog of a user's ceiling is
-- at `write`, and seeks it here instead of scanning a team's read-only
-- catalogs. Built CONCURRENTLY, one statement in a non-transactional
-- migration, for the reasons `0025_catalogs_member_reach.sql` gives.
CREATE INDEX CONCURRENTLY IF NOT EXISTS catalogs_member_write
    ON catalogs (owner_team_id) WHERE member_access = 'write';

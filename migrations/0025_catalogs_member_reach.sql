-- no-transaction
-- The catalogs a team's members reach through the team, by name (issue
-- #728): a user's ceiling is resolved per catalog, and the catalogs a
-- console session reaches are listed a page at a time per owner team
-- (`store::catalogs::ceiling_page`), never read whole. Catalogs with
-- `member_access = 'none'` give members nothing, so they are left out.
--
-- Built CONCURRENTLY, so replicas still serving during a rolling upgrade
-- keep writing catalogs while it builds (a plain CREATE INDEX holds a SHARE
-- lock on `catalogs` for the whole build). That cannot run in a transaction,
-- hence `no-transaction` above (it must be the file's first line), and the
-- file holds this one statement only: a multi-statement query is an
-- implicit transaction. A build that failed part-way leaves an INVALID
-- index; `db::migrate` drops it before the migrations run, so the retry
-- builds it again (`IF NOT EXISTS` would otherwise keep the invalid one).
CREATE INDEX CONCURRENTLY IF NOT EXISTS catalogs_member_reach
    ON catalogs (owner_team_id, name) WHERE member_access <> 'none';

-- The catalogs a team's members reach through the team, by name (issue
-- #728): a user's ceiling is resolved per catalog, and the catalogs a
-- console session reaches are listed a page at a time per owner team
-- (`store::catalogs::ceiling_page`), never read whole. Catalogs with
-- `member_access = 'none'` give members nothing, so they are left out.
CREATE INDEX catalogs_member_reach ON catalogs (owner_team_id, name)
    WHERE member_access <> 'none';

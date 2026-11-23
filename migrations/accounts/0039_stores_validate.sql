-- Validates what 0036 to 0038 added NOT VALID (issue #965): the two foreign
-- keys to `stores` and the two audit CHECKs. Its own migration, so its own
-- transaction: VALIDATE CONSTRAINT takes only SHARE UPDATE EXCLUSIVE on the
-- table it scans (plus ROW SHARE on `stores`), so reads and writes go on
-- while the existing rows are checked, as in 0032. Every existing row
-- passes: every catalog is on `main`, no team is pinned, and no audit row
-- has the new target kind.
ALTER TABLE teams VALIDATE CONSTRAINT teams_store_fkey;
ALTER TABLE catalogs VALIDATE CONSTRAINT catalogs_store_fkey;
ALTER TABLE audit_entries VALIDATE CONSTRAINT audit_entries_target_kind_check;
ALTER TABLE audit_entries VALIDATE CONSTRAINT audit_entries_target_shape;

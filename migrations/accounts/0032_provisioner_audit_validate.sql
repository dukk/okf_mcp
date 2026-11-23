-- Validates the CHECK 0031 added `NOT VALID` (issue #423). Its own
-- migration, so its own transaction: VALIDATE CONSTRAINT takes only a SHARE
-- UPDATE EXCLUSIVE lock, so the audit log can still be read and written
-- while the existing rows are scanned. Every existing row already passes
-- (the old CHECK allowed every kind but `route`). It cannot go in 0031:
-- sqlx runs each migration in one transaction, so a VALIDATE there would
-- still scan under that file's ACCESS EXCLUSIVE lock.
ALTER TABLE audit_entries VALIDATE CONSTRAINT audit_entries_target_kind_check;

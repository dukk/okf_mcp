-- Validates the two audit CHECKs 0036 added `NOT VALID` (issue #965). Its
-- own migration, so its own transaction: VALIDATE CONSTRAINT takes only a
-- SHARE UPDATE EXCLUSIVE lock, so the audit log can still be read and
-- written while the existing rows are scanned. Every existing row passes
-- (no row has the new target kind, and the shape check differs from 0012's
-- only for it).
ALTER TABLE audit_entries VALIDATE CONSTRAINT audit_entries_target_kind_check;
ALTER TABLE audit_entries VALIDATE CONSTRAINT audit_entries_target_shape;

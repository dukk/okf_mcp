-- Validates the CHECK 0029 added `NOT VALID` (issue #793). Its own
-- migration, so its own transaction: VALIDATE CONSTRAINT takes only a SHARE
-- UPDATE EXCLUSIVE lock, so feedback can still be read and written while
-- the existing rows are scanned. Every existing row already passes (the old
-- CHECK allowed only `api_key` and `oauth`).
ALTER TABLE feedback VALIDATE CONSTRAINT feedback_credential_kind_check;

-- Feedback from the web console (issue #793): `POST /api/v1/feedback`
-- stores its submissions in the same `feedback` table as the
-- `send_feedback` tool, with `credential_kind` `session`. Its `credential`
-- is `user:<users.id>`, so the per-credential rate limit counts the
-- signed-in user's console submissions together, whichever session sent
-- them.
--
-- The wider CHECK is added `NOT VALID`: new and updated rows are checked at
-- once, but the existing rows are not scanned while this ALTER TABLE holds
-- its ACCESS EXCLUSIVE lock (feedback accumulates, and failed rows are kept).
-- `0030_feedback_validate_credential_kind.sql` validates it in its own
-- transaction, under a lock that lets reads and writes go on. It cannot go
-- in this file: sqlx runs each migration in one transaction, so a VALIDATE
-- here would still scan under this lock.
ALTER TABLE feedback
    DROP CONSTRAINT feedback_credential_kind_check,
    ADD CONSTRAINT feedback_credential_kind_check
        CHECK (credential_kind IN ('api_key', 'oauth', 'session')) NOT VALID;

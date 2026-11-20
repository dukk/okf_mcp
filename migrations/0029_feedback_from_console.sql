-- Feedback from the web console (issue #793): `POST /api/v1/feedback`
-- stores its submissions in the same `feedback` table as the
-- `send_feedback` tool, with `credential_kind` `session`. Its `credential`
-- is `user:<users.id>`, so the per-credential rate limit counts the
-- signed-in user's console submissions together, whichever session sent
-- them.
ALTER TABLE feedback
    DROP CONSTRAINT feedback_credential_kind_check,
    ADD CONSTRAINT feedback_credential_kind_check
        CHECK (credential_kind IN ('api_key', 'oauth', 'session'));

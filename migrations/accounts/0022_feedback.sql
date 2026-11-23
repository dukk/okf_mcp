-- Feedback sent with the `send_feedback` tool (issue #674). The tool inserts
-- a row and, in the same transaction, one `send_feedback` job in
-- `apalis.jobs` naming it; the feedback worker forwards the row to the
-- operator's Forminit form and records the outcome here. The row holds
-- exactly what is forwarded, plus `credential` (the calling key's id, for
-- the per-credential rate limit; never forwarded) and the delivery state.
CREATE TABLE feedback (
    id              text PRIMARY KEY,
    -- `api_keys.id` of the credential that sent it. Not a foreign key: a
    -- revoked or deleted key must not take queued feedback with it.
    credential      text NOT NULL,
    -- The credential's user slug, forwarded as the sender's `userId`.
    user_slug       text NOT NULL,
    message         text NOT NULL CHECK (char_length(message) BETWEEN 1 AND 4000),
    kind            text NOT NULL CHECK (kind IN ('bug', 'confusing', 'missing', 'praise', 'other')),
    -- `from` in the tool and the submission; FROM is an SQL keyword.
    origin          text NOT NULL CHECK (origin IN ('user', 'agent')),
    tool            text,
    rating          smallint CHECK (rating BETWEEN 1 AND 5),
    server_version  text NOT NULL,
    client          text NOT NULL,
    credential_kind text NOT NULL CHECK (credential_kind IN ('api_key', 'oauth')),
    status          text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed')),
    attempts        integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    -- The last failure: Forminit's error code, or why a try failed.
    last_error      text,
    -- Forminit's `submission.hashId`, once sent.
    submission_id   text,
    created_at      timestamptz NOT NULL,
    -- When it was sent or given up on.
    finished_at     timestamptz CHECK (finished_at IS NULL OR finished_at >= created_at)
);
-- The rate limit counts a credential's rows of the last hour.
CREATE INDEX feedback_credential_created ON feedback (credential, created_at);
-- The poll loop's prune finds sent rows past the retention period.
CREATE INDEX feedback_sent_finished ON feedback (finished_at) WHERE status = 'sent';

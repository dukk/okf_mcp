-- no-transaction
-- The user deletion removes the user's undelivered feedback (`pending` and
-- `failed`, issue #698) while it holds the user's row lock and the admin
-- rows' locks, so the lookup by `user_slug` must not scan the table. Partial,
-- because only undelivered rows are deleted: it stays as small as the
-- backlog. Built CONCURRENTLY, one statement in a non-transactional
-- migration, as `0033_feedback_finished_index.sql` does.
CREATE INDEX CONCURRENTLY IF NOT EXISTS feedback_undelivered_user
    ON feedback (user_slug) WHERE status IN ('pending', 'failed');

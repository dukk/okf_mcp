-- no-transaction
-- The sent-only prune index that `0033_feedback_finished_index.sql`
-- replaces (issue #698). Dropped CONCURRENTLY so a reader of `feedback` is
-- not blocked; its own migration, because a non-transactional file may hold
-- only one such statement.
DROP INDEX CONCURRENTLY IF EXISTS feedback_sent_finished;

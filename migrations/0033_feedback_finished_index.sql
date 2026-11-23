-- no-transaction
-- Failed feedback is pruned after the same retention as sent feedback
-- (issue #698), so the prune's index covers both statuses. Built
-- CONCURRENTLY, one statement in a non-transactional migration, so writers
-- (`send_feedback`, the worker) are not blocked while the existing rows are
-- scanned: failed rows were kept without limit until now, and the retention
-- can be ten years. A build that fails part-way leaves an INVALID index,
-- which `db::migrate` drops before it tries again (`CONCURRENT_INDEXES`).
CREATE INDEX CONCURRENTLY IF NOT EXISTS feedback_finished
    ON feedback (finished_at) WHERE status IN ('sent', 'failed');

-- Failed feedback is pruned after the same retention as sent feedback
-- (issue #698), so the prune's index covers both. The table holds a month of
-- feedback at most, so the plain rebuild is quick.
DROP INDEX feedback_sent_finished;
CREATE INDEX feedback_finished ON feedback (finished_at) WHERE status IN ('sent', 'failed');

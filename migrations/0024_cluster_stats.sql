-- One row: the cluster-wide counts the `cluster_stats` housekeeping job
-- computes once a minute (issue #693), read by every replica's gauges, so
-- every replica reports the same values (M18 spec Decision 16).
CREATE TABLE cluster_stats (
    -- Always true: the CHECK and the primary key allow one row only.
    id              boolean PRIMARY KEY DEFAULT true CHECK (id),
    -- Changesets in state 'open', in every catalog.
    open_changesets bigint NOT NULL,
    -- Mail jobs not yet sent, by status: {"pending": 2, "failed": 1, ...}.
    mail_queue      jsonb  NOT NULL,
    computed_at     timestamptz NOT NULL
);

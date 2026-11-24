-- Each store's open changeset count (plan Task 16, issue #976). The
-- `cluster_stats` housekeeping task counts one store per job (keyed
-- `cluster_stats:<store>:<slot>`) and writes that store's row here; the
-- cluster's job sums the rows into `cluster_stats.open_changesets`. A store
-- whose count failed keeps its last row, so the sum carries its last known
-- count rather than dropping it. The rows go with their store.
CREATE TABLE cluster_store_stats (
    store           text PRIMARY KEY REFERENCES stores (name) ON DELETE CASCADE,
    -- Changesets in state 'open', in every catalog on the store.
    open_changesets bigint NOT NULL CHECK (open_changesets >= 0),
    computed_at     timestamptz NOT NULL
);

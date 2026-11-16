-- The poll pass counts open changesets per catalog (`changesets::open_counts`,
-- for the `okf.changesets.open` gauge). Published and discarded changesets are
-- kept, so without this the count reads the whole table each pass: the
-- partial index `changesets_one_open_per_label` that once covered open rows
-- went in 0004. This one holds open rows only, so the count stays a scan of
-- the few of them.
CREATE INDEX changesets_open ON changesets (catalog_id) WHERE state = 'open';

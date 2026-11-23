-- The registry's placement and provisioning (spec Decisions 6, 11): where a
-- catalog lives, and whether its store side exists yet.
--
-- Both defaults keep release-1 replicas working during the rollout (P11):
-- their creates name neither column, and land on main, provisioned. New code
-- always passes both explicitly.
--
-- Locks. This is the only statement of the file, so ACCESS EXCLUSIVE on
-- `catalogs` is held for the instant it takes to change the catalog and
-- nothing more:
--   * Both columns have a default that is not volatile (`'main'`, and
--     `now()`, which is STABLE), so PostgreSQL 11 and later keep the default
--     in the catalog and rewrite no row; every existing row reads as `main`,
--     provisioned at the moment this ran. There is no backfill UPDATE, so no
--     row is rewritten and none scanned.
--   * The foreign key is added NOT VALID, so the existing rows are not
--     checked under the lock; 0039 validates it under SHARE UPDATE
--     EXCLUSIVE (writes continue). New rows are checked at once.
-- The index that finds half-created catalogs is built CONCURRENTLY by 0040.
ALTER TABLE catalogs
    ADD COLUMN store text NOT NULL DEFAULT 'main',
    ADD COLUMN provisioned_at timestamptz DEFAULT now(),
    ADD CONSTRAINT catalogs_store_fkey FOREIGN KEY (store) REFERENCES stores (name)
        ON DELETE RESTRICT NOT VALID;

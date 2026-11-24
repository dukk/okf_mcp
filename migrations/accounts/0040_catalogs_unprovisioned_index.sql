-- no-transaction
-- The provisioning job finds half-created catalogs by this index (plan Task
-- 10b): the rows with `provisioned_at` NULL, oldest first. Built
-- CONCURRENTLY, as 0025 was, so replicas serving during a rolling upgrade
-- keep writing catalogs while it builds (a plain CREATE INDEX holds a SHARE
-- lock on `catalogs` for the whole build). That cannot run in a transaction,
-- hence `no-transaction` above, and the file holds this one statement only.
-- A build that failed part-way leaves an INVALID index, which `db::migrate`
-- drops before the migrations run (`CONCURRENT_INDEXES`).
CREATE INDEX CONCURRENTLY IF NOT EXISTS catalogs_unprovisioned
    ON catalogs (created_at) WHERE provisioned_at IS NULL;

-- The split (catalog stores plan, Task 11; spec decisions 5, 9, 16-18):
-- the catalog's own tables move into schema `catalog`, the registry
-- (`catalogs`) stays in `public`, and every foreign key that crossed the
-- line is dropped or re-pointed.
--
-- NOT ROLLING-SAFE (plan decision P10). Release-2 replicas query these
-- tables in `public` and read `catalogs.generation`, and this migration
-- removes both. It takes ACCESS EXCLUSIVE locks (SET SCHEMA, DROP COLUMN,
-- the foreign keys' validation scans) and is meant for the release-3
-- maintenance window: snapshot the database, scale `serve` to zero, run
-- `okf-mcp migrate`, start the new release. It is correct on an empty and
-- on a populated database (the tests run both).
--
-- Every constraint name below was read from `pg_constraint` on a database
-- migrated through 0040 (they are Postgres's defaults).

CREATE SCHEMA catalog;

-- The catalog's own state row: the publish counter, and the row writes
-- lock to hold off deletion (it replaces the `catalogs` row in both roles).
CREATE TABLE catalog.catalog_state (
    id         bigint PRIMARY KEY,
    generation bigint NOT NULL DEFAULT 0,
    -- Set by a delete under the row lock (P12, Task 13); every writer
    -- refuses a row that has it.
    deleted_at timestamptz
);

-- One row per catalog that has a store side: every provisioned catalog,
-- and a half-created one whose starter files were already written (its
-- documents need the row for their foreign key). A half-created catalog
-- with no documents gets none, so the recovery task still finds its store
-- side missing and removes it (`catalogs::store_side_exists`).
INSERT INTO catalog.catalog_state (id, generation)
    SELECT c.id, c.generation FROM catalogs c
     WHERE c.provisioned_at IS NOT NULL
        OR EXISTS (SELECT 1 FROM documents d WHERE d.catalog_id = c.id);

-- `SET SCHEMA` moves a table with its indexes, constraints between moved
-- tables, triggers and owned sequences; it rewrites no data.
ALTER TABLE documents                SET SCHEMA catalog;
ALTER TABLE revisions                SET SCHEMA catalog;
ALTER TABLE changesets               SET SCHEMA catalog;
ALTER TABLE changeset_entries        SET SCHEMA catalog;
ALTER TABLE maintenance_state        SET SCHEMA catalog;
ALTER TABLE maintenance_runs         SET SCHEMA catalog;
ALTER TABLE maintenance_work         SET SCHEMA catalog;
ALTER TABLE maintenance_findings     SET SCHEMA catalog;
ALTER TABLE maintenance_evidence     SET SCHEMA catalog;
ALTER TABLE maintenance_dispositions SET SCHEMA catalog;
ALTER TABLE maintenance_requests     SET SCHEMA catalog;

-- The catalog's tables reference `catalog_state`, not the registry.
ALTER TABLE catalog.documents
    DROP CONSTRAINT documents_catalog_id_fkey,
    ADD CONSTRAINT documents_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE;
ALTER TABLE catalog.revisions
    DROP CONSTRAINT revisions_catalog_id_fkey,
    ADD CONSTRAINT revisions_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE;
ALTER TABLE catalog.changesets
    DROP CONSTRAINT changesets_catalog_id_fkey,
    ADD CONSTRAINT changesets_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE;
ALTER TABLE catalog.maintenance_state
    DROP CONSTRAINT maintenance_state_catalog_id_fkey,
    ADD CONSTRAINT maintenance_state_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE;
ALTER TABLE catalog.maintenance_runs
    DROP CONSTRAINT maintenance_runs_catalog_id_fkey,
    ADD CONSTRAINT maintenance_runs_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE;
ALTER TABLE catalog.maintenance_work
    DROP CONSTRAINT maintenance_work_catalog_id_fkey,
    ADD CONSTRAINT maintenance_work_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE;
ALTER TABLE catalog.maintenance_findings
    DROP CONSTRAINT maintenance_findings_catalog_id_fkey,
    ADD CONSTRAINT maintenance_findings_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE;
ALTER TABLE catalog.maintenance_evidence
    DROP CONSTRAINT maintenance_evidence_catalog_id_fkey,
    ADD CONSTRAINT maintenance_evidence_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE;
ALTER TABLE catalog.maintenance_dispositions
    DROP CONSTRAINT maintenance_dispositions_catalog_id_fkey,
    ADD CONSTRAINT maintenance_dispositions_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE;
ALTER TABLE catalog.maintenance_requests
    DROP CONSTRAINT maintenance_requests_catalog_id_fkey,
    ADD CONSTRAINT maintenance_requests_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE;

-- Foreign keys across the line become plain values (spec Decision 9). A
-- key id left on a changeset or a run after its key is deleted is read as
-- "a deleted key": the readers look labels up by id and find none.
ALTER TABLE catalog.changesets DROP CONSTRAINT changesets_key_id_fkey;
ALTER TABLE catalog.maintenance_runs DROP CONSTRAINT maintenance_runs_key_id_fkey;

-- The maintenance audit triggers read and write account tables, so they
-- cannot follow `maintenance_runs` into the store (review of PR #956).
-- Rust records `catalog.maintenance_run` instead
-- (`store::maintenance::record_ended_runs`); Task 12 moves that write
-- into the store transaction.
DROP TRIGGER maintenance_runs_audit_end ON catalog.maintenance_runs;
DROP TRIGGER maintenance_runs_audit_empty ON catalog.maintenance_runs;
DROP FUNCTION audit_maintenance_run();

-- The generation lives in `catalog.catalog_state` now.
ALTER TABLE catalogs DROP COLUMN generation;
-- Deleted but not yet cleaned up (P6; written by Task 13).
ALTER TABLE catalogs ADD COLUMN deleted_at timestamptz;

-- Cleanup owed on stores after an account-side deletion (P13), written in
-- the deleting transaction (Task 13). One row per subject per store; the
-- ids are kept here because the key or catalog they name is gone.
CREATE TABLE store_cleanups (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    store      text NOT NULL REFERENCES stores (name) ON DELETE RESTRICT,
    kind       text NOT NULL CHECK (kind IN ('key', 'catalog')),
    subject    text NOT NULL,
    created_at timestamptz NOT NULL,
    attempts   integer NOT NULL DEFAULT 0,
    UNIQUE (store, kind, subject)
);

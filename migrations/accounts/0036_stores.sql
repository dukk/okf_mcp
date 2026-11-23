-- Stores: the databases catalog data can live in (spec
-- 2026-11-23-okf-catalog-stores-design.md, Decisions 1-4, 6, 11).
--
-- Additive, and compatible with release-1 replicas during a rolling deploy
-- (plan decision P11). Nothing here is credential material: a `host` row
-- holds a reference to a credential (an environment variable name, a secret
-- ARN, or an IAM identity), never the credential.
--
-- The registry changes are split over 0036 to 0040 so that no statement
-- rewrites or scans a big table while holding ACCESS EXCLUSIVE on it. This
-- file creates `stores` (new, nobody waits on it) and then does the one
-- statement that locks an existing table, last, so the lock is held only
-- until this file commits:
--   * `ALTER TABLE teams ADD COLUMN store ...`: ACCESS EXCLUSIVE on `teams`,
--     metadata-only (a nullable column with no default). The foreign key is
--     added NOT VALID, which skips the scan of `teams`; 0039 validates it
--     under a weaker lock.
CREATE TABLE stores (
    name            text PRIMARY KEY CHECK (name ~ '^[a-z0-9-]+$'),
    location_kind   text NOT NULL CHECK (location_kind IN ('account-database', 'host')),
    host            text,
    port            integer CHECK (port BETWEEN 1 AND 65535),
    database        text,
    credential_kind text CHECK (credential_kind IN ('iam', 'secrets-manager', 'env')),
    credential_ref  text,
    created_at      timestamptz NOT NULL,
    CHECK ((location_kind = 'account-database') =
           (host IS NULL AND port IS NULL AND database IS NULL
            AND credential_kind IS NULL AND credential_ref IS NULL)),
    CHECK ((location_kind = 'host') =
           (host IS NOT NULL AND port IS NOT NULL AND database IS NOT NULL
            AND credential_kind IS NOT NULL AND credential_ref IS NOT NULL))
);
INSERT INTO stores (name, location_kind, created_at)
    VALUES ('main', 'account-database', now());
-- No two store names for one database (review of PR #956): only one
-- account-database row, and one row per host, port and database name.
-- A later task adds the check for aliases these can't see (two host names
-- for one server, or a host row pointing at the account database).
CREATE UNIQUE INDEX stores_one_account_database ON stores (location_kind)
    WHERE location_kind = 'account-database';
CREATE UNIQUE INDEX stores_one_row_per_database ON stores (lower(host), port, database)
    WHERE location_kind = 'host';

-- The default store for new catalogs (spec Decision 6).
INSERT INTO server_settings (key, value, updated_at, updated_by)
    VALUES ('default_store', '"main"'::jsonb, now(), 'system:migration')
    ON CONFLICT (key) DO NOTHING;

-- A team's pin (spec Decision 6). RESTRICT: a pinned store cannot be removed.
ALTER TABLE teams
    ADD COLUMN store text,
    ADD CONSTRAINT teams_store_fkey FOREIGN KEY (store) REFERENCES stores (name)
        ON DELETE RESTRICT NOT VALID;

-- Stores: the databases catalog data can live in (spec
-- 2026-11-23-okf-catalog-stores-design.md, Decisions 1-4, 6, 11).
--
-- Additive, and compatible with release-1 replicas during a rolling deploy
-- (plan decision P11): an older replica's `INSERT INTO catalogs` names
-- neither new column, and lands on `main`, provisioned. Nothing here is
-- credential material: a `host` row holds a reference to a credential
-- (an environment variable name, a secret ARN, or an IAM identity), never
-- the credential.
--
-- Locks. sqlx runs a migration in one transaction, so each lock is held
-- until it commits.
--   * `stores` is new, so its statements wait for nobody.
--   * `ALTER TABLE teams ADD COLUMN store ... REFERENCES` takes ACCESS
--     EXCLUSIVE on `teams` for a moment: a nullable column with no default
--     is metadata-only, and the foreign key finds only NULLs.
--   * `ALTER TABLE catalogs ADD COLUMN` takes ACCESS EXCLUSIVE on
--     `catalogs`. Both columns have a default that is not volatile
--     (`'main'`, and `now()`, which is STABLE), so Postgres 11 and later
--     store the default in the catalog and rewrite no row. The new foreign
--     key checks the one value `'main'` against `stores`. `teams` is locked
--     before `catalogs`, the order every writer takes them in.
--   * The `UPDATE catalogs` rewrites each row once, under the same
--     transaction, so a registry row is never visible without its
--     `provisioned_at`. The registry has one row per catalog, so the
--     rewrite is small.
--   * The two audit CHECKs are added NOT VALID, which skips the scan of the
--     whole audit log under the ACCESS EXCLUSIVE lock; the next migration
--     (0037) validates them in a transaction of its own, as 0031 and 0032
--     did.
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
ALTER TABLE teams ADD COLUMN store text REFERENCES stores (name) ON DELETE RESTRICT;

-- The registry's placement and provisioning (spec Decisions 6, 11).
-- Both defaults keep release-1 replicas working during the rollout (P11):
-- their creates name neither column, and land on main, provisioned.
-- New code always passes both explicitly.
ALTER TABLE catalogs
    ADD COLUMN store text NOT NULL DEFAULT 'main'
        REFERENCES stores (name) ON DELETE RESTRICT,
    ADD COLUMN provisioned_at timestamptz DEFAULT now();
UPDATE catalogs SET provisioned_at = created_at;
-- The provisioning job finds half-created catalogs by this.
CREATE INDEX catalogs_unprovisioned ON catalogs (created_at) WHERE provisioned_at IS NULL;

-- Audit entries about a store (`store.added`, ...): target_kind gains
-- 'store', whose target is its name and nothing else. Both supersets of the
-- old checks (no stored row has the new kind), added NOT VALID; 0037
-- validates them. The shape CHECK's text is 0012's, with one new arm.
ALTER TABLE audit_entries DROP CONSTRAINT audit_entries_target_kind_check;
ALTER TABLE audit_entries ADD CONSTRAINT audit_entries_target_kind_check CHECK (
    target_kind IN ('user', 'team', 'catalog', 'key', 'grant', 'code', 'setting', 'unlock',
                    'route', 'store')
) NOT VALID;
ALTER TABLE audit_entries DROP CONSTRAINT audit_entries_target_shape;
ALTER TABLE audit_entries ADD CONSTRAINT audit_entries_target_shape CHECK (
    CASE target_kind
        WHEN 'user' THEN target_person_id IS NOT NULL AND target_name IS NULL
        WHEN 'team' THEN target_team_ref IS NOT NULL AND target_name IS NULL
        WHEN 'key' THEN target_credential_ref IS NOT NULL AND target_name IS NULL
        WHEN 'grant' THEN target_credential_ref IS NOT NULL AND target_name IS NULL
        WHEN 'code' THEN target_credential_ref IS NOT NULL AND target_name IS NULL
        WHEN 'store' THEN target_name IS NOT NULL AND target_team_ref IS NULL
                          AND target_credential_ref IS NULL AND target_person_id IS NULL
        ELSE target_name IS NOT NULL
    END
) NOT VALID;

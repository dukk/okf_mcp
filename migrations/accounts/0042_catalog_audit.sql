-- Catalog-event audit entries on the store (catalog stores plan, Task 12;
-- spec Decision 10; plan decisions P4 and P7).
--
-- The six actions a catalog transaction records (`catalog.published`,
-- `catalog.imported`, `catalog.repaired`, `catalog.reverted`,
-- `catalog.changeset_discarded`, `catalog.maintenance_run`) are written in
-- the store's own transaction from now on, so the entry commits or rolls
-- back with the change it records. Entries written before this migration
-- stay in `public.audit_entries` (P7): that table's append-only trigger
-- forbids deleting recent rows, and suspending it would weaken the
-- guarantee it exists for. The audit view reads both tables, so each entry
-- appears once.
--
-- The identity columns hold the account database's `audit_people`,
-- `audit_credentials` and `audit_teams` ids as plain values: no foreign key
-- crosses the line (spec Decision 9). The writer resolves them on the
-- account database before the store transaction begins; the view resolves
-- names through the account database when it reads, so deleting a user
-- (which clears the name in `audit_people`) clears it in these entries too.
--
-- Not named `audit_entries`: no table name exists in both schemas (P1), so
-- an unqualified audit statement sent to the wrong pool fails instead of
-- touching the other log.
--
-- The table is new, so this migration is rolling-safe: release-2 replicas
-- never name it.
CREATE TABLE catalog.catalog_audit_entries (
    id                    bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at                    timestamptz NOT NULL,
    -- The catalog's id (`catalogs.id`, `catalog_state.id`) as a plain value:
    -- the entry outlives the catalog (P5).
    catalog_id            bigint NOT NULL,
    action                text NOT NULL CHECK (action ~ '^[a-z]+\.[a-z_]+$'),
    source                text NOT NULL CHECK (source IN ('cli', 'api', 'console', 'system')),
    actor_credential_ref  bigint,
    actor_person_id       bigint,
    -- Never a person's name: 'console session', 'api key', 'oauth grant',
    -- 'cli:<os user>' or 'system:<what>' (as in the account table).
    actor_label           text NOT NULL,
    -- An admin credential or a provisioner: owners see "an operator".
    actor_operator        boolean NOT NULL,
    client_ip             inet,
    target_name           text NOT NULL,
    -- A user the entry is also about (a discard when its user is deleted),
    -- and whether that user was an admin then (hidden from team owners).
    target_person_id      bigint,
    target_operator       boolean NOT NULL DEFAULT false,
    -- The `audit_teams` ids the entry belongs to (the scope of an owner's view).
    teams                 bigint[] NOT NULL DEFAULT '{}',
    details               jsonb NOT NULL DEFAULT '{}'::jsonb
                              CHECK (jsonb_typeof(details) = 'object')
);
-- `(at, id)` serves the view's newest-first page and the retention prune.
CREATE INDEX catalog_audit_entries_at ON catalog.catalog_audit_entries (at, id);
CREATE INDEX catalog_audit_entries_teams ON catalog.catalog_audit_entries USING gin (teams);

-- Append-only, as the account table is. A store has no `server_settings`,
-- so the prune job passes the retention period in a transaction-local
-- setting (`SET LOCAL okf.audit_retention_days = N`); without it the
-- default is 400 days. Never less than 30, whatever is passed.
CREATE FUNCTION catalog.catalog_audit_entries_append_only() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    retention integer;
BEGIN
    IF TG_OP <> 'DELETE' THEN
        RAISE EXCEPTION 'audit entries are append-only' USING ERRCODE = 'insufficient_privilege';
    END IF;
    -- A value that is not a number is the same as none: the default.
    BEGIN
        retention := NULLIF(current_setting('okf.audit_retention_days', true), '')::integer;
    EXCEPTION WHEN invalid_text_representation OR numeric_value_out_of_range THEN
        retention := NULL;
    END;
    retention := GREATEST(30, COALESCE(retention, 400));
    IF OLD.at >= now() - make_interval(days => retention) THEN
        RAISE EXCEPTION 'audit entry % is inside the retention period', OLD.id
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER catalog_audit_entries_append_only BEFORE UPDATE OR DELETE ON catalog.catalog_audit_entries
    FOR EACH ROW EXECUTE FUNCTION catalog.catalog_audit_entries_append_only();
CREATE TRIGGER catalog_audit_entries_no_truncate BEFORE TRUNCATE ON catalog.catalog_audit_entries
    FOR EACH STATEMENT EXECUTE FUNCTION catalog.catalog_audit_entries_append_only();

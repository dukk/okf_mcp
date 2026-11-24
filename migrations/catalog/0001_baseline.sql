-- The catalog store baseline (catalog stores plan, Task 14; spec "Two
-- migration folders"): the whole `catalog` schema of a store, exactly as the
-- account migrations 0041 and 0042 leave it in the account database.
--
-- Generated from `pg_dump --schema-only --schema=catalog` of a database
-- migrated through 0042, then rewritten as readable SQL. Every name (table,
-- column order, constraint, index, sequence, trigger, function) matches the
-- dump, so a store created from this file and the account database adopted
-- after 0042 have the same schema (`adopted_schema_equals_a_fresh_store_schema`).
-- Change the catalog schema with a new file in this folder, never here: this
-- file's checksum is what adoption records.
--
-- Two ways this runs:
-- - a new store: `db::migrate_store` creates schema `catalog`, then runs it;
-- - the account database after 0042: adoption records it as applied without
--   running it, since 0041 and 0042 already built these objects there.
--
-- Every name is schema-qualified, because the account database's migration
-- connection has `search_path = public`. No object refers to `public`: a
-- store holds no account table.

-- The catalog's own state row: the publish counter, and the row writes lock
-- to hold off deletion. `id` is the registry's `catalogs.id`.
CREATE TABLE catalog.catalog_state (
    id         bigint NOT NULL,
    generation bigint DEFAULT 0 NOT NULL,
    -- Set by a delete under the row lock (P12).
    deleted_at timestamptz,
    CONSTRAINT catalog_state_pkey PRIMARY KEY (id)
);

-- A catalog's current files.
CREATE TABLE catalog.documents (
    catalog_id   bigint NOT NULL,
    path         text NOT NULL,
    content      bytea NOT NULL,
    executable   boolean NOT NULL,
    revision     integer NOT NULL,
    deleted_at   timestamptz,
    updated_at   timestamptz NOT NULL,
    updated_by   text NOT NULL,
    kind         text,
    concept_type text,
    title        text,
    status       text,
    tags         text[] DEFAULT '{}'::text[] NOT NULL,
    CONSTRAINT documents_kind_check
        CHECK (kind = ANY (ARRAY['concept'::text, 'index'::text, 'log'::text, 'machinery'::text])),
    CONSTRAINT documents_revision_check CHECK (revision > 0),
    CONSTRAINT documents_pkey PRIMARY KEY (catalog_id, path),
    CONSTRAINT documents_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE
);

-- Staged edits. `key_id` names an account-database key as a plain value
-- (spec Decision 9).
CREATE TABLE catalog.changesets (
    id         bigint GENERATED ALWAYS AS IDENTITY,
    catalog_id bigint NOT NULL,
    key_id     text,
    actor      text NOT NULL,
    state      text NOT NULL,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    version    bigint DEFAULT 0 NOT NULL,
    handle     text DEFAULT (gen_random_uuid())::text NOT NULL,
    CONSTRAINT changesets_state_check
        CHECK (state = ANY (ARRAY['open'::text, 'published'::text, 'discarded'::text])),
    CONSTRAINT changesets_pkey PRIMARY KEY (id),
    CONSTRAINT changesets_handle_key UNIQUE (handle),
    CONSTRAINT changesets_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE
);
CREATE INDEX changesets_catalog_id ON catalog.changesets USING btree (catalog_id);
CREATE INDEX changesets_key_id ON catalog.changesets USING btree (key_id)
    WHERE (key_id IS NOT NULL);
CREATE INDEX changesets_open ON catalog.changesets USING btree (catalog_id)
    WHERE (state = 'open'::text);

CREATE TABLE catalog.changeset_entries (
    changeset_id  bigint NOT NULL,
    path          text NOT NULL,
    base_revision integer,
    content       bytea,
    executable    boolean DEFAULT false NOT NULL,
    replay        jsonb,
    CONSTRAINT changeset_entries_pkey PRIMARY KEY (changeset_id, path),
    CONSTRAINT changeset_entries_changeset_id_fkey FOREIGN KEY (changeset_id)
        REFERENCES catalog.changesets (id) ON DELETE CASCADE
);

-- Every published version of every file.
CREATE TABLE catalog.revisions (
    catalog_id   bigint NOT NULL,
    path         text NOT NULL,
    revision     integer NOT NULL,
    op           text NOT NULL,
    content      bytea,
    executable   boolean NOT NULL,
    actor        text NOT NULL,
    changeset_id bigint,
    message      text NOT NULL,
    created_at   timestamptz NOT NULL,
    CONSTRAINT revisions_op_check
        CHECK (op = ANY (ARRAY['create'::text, 'update'::text, 'delete'::text, 'import'::text])),
    CONSTRAINT revisions_catalog_id_path_revision_key PRIMARY KEY (catalog_id, path, revision),
    CONSTRAINT revisions_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE,
    CONSTRAINT revisions_changeset_id_fkey FOREIGN KEY (changeset_id)
        REFERENCES catalog.changesets (id) ON DELETE SET NULL
);
CREATE INDEX revisions_catalog_recent
    ON catalog.revisions USING btree (catalog_id, created_at, path, revision);
CREATE INDEX revisions_changeset_id ON catalog.revisions USING btree (changeset_id)
    WHERE (changeset_id IS NOT NULL);

-- Maintenance: per-catalog cursors, runs, their work items, findings,
-- evidence, dispositions and idempotent requests.
CREATE TABLE catalog.maintenance_state (
    catalog_id          bigint NOT NULL,
    changed_cursor      text DEFAULT ''::text NOT NULL,
    expiry_cursor       text DEFAULT ''::text NOT NULL,
    verification_cursor text DEFAULT ''::text NOT NULL,
    next_aging_group    text DEFAULT 'expiry'::text NOT NULL,
    updated_at          timestamptz NOT NULL,
    CONSTRAINT maintenance_state_next_aging_group_check
        CHECK (next_aging_group = ANY (ARRAY['expiry'::text, 'verification'::text])),
    CONSTRAINT maintenance_state_pkey PRIMARY KEY (catalog_id),
    CONSTRAINT maintenance_state_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE
);

CREATE TABLE catalog.maintenance_runs (
    id            text NOT NULL,
    catalog_id    bigint NOT NULL,
    -- An account-database key as a plain value (spec Decision 9).
    key_id        text,
    credential_id text NOT NULL,
    actor         text NOT NULL,
    state         text NOT NULL,
    deadline      timestamptz NOT NULL,
    created_at    timestamptz NOT NULL,
    ended_at      timestamptz,
    end_reason    text,
    primary_count integer DEFAULT 0 NOT NULL,
    CONSTRAINT maintenance_runs_credential_id_check CHECK (credential_id <> ''::text),
    CONSTRAINT maintenance_runs_primary_count_check
        CHECK ((primary_count >= 0) AND (primary_count <= 50)),
    CONSTRAINT maintenance_runs_state_check
        CHECK (state = ANY (ARRAY['running'::text, 'completed'::text, 'failed'::text, 'cancelled'::text])),
    CONSTRAINT maintenance_runs_pkey PRIMARY KEY (id),
    CONSTRAINT maintenance_runs_catalog_id_id_key UNIQUE (catalog_id, id),
    CONSTRAINT maintenance_runs_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE
);
CREATE INDEX maintenance_runs_catalog_created
    ON catalog.maintenance_runs USING btree (catalog_id, created_at, id);
CREATE INDEX maintenance_runs_key_id ON catalog.maintenance_runs USING btree (key_id)
    WHERE (key_id IS NOT NULL);

CREATE TABLE catalog.maintenance_work (
    id               text NOT NULL,
    run_id           text NOT NULL,
    catalog_id       bigint NOT NULL,
    path             text NOT NULL,
    revision         integer NOT NULL,
    lanes            text[] NOT NULL,
    aging_group      text,
    outcome          text,
    assessment       text,
    reason           text,
    summary          text,
    lease_token      text NOT NULL,
    lease_expires_at timestamptz NOT NULL,
    attempted_at     timestamptz NOT NULL,
    ended_at         timestamptz,
    reported_tokens  bigint,
    CONSTRAINT maintenance_work_aging_group_check
        CHECK (aging_group = ANY (ARRAY['expiry'::text, 'verification'::text])),
    CONSTRAINT maintenance_work_assessment_check
        CHECK (assessment = ANY (ARRAY['supported_current'::text, 'outdated'::text,
                                       'unverifiable'::text, 'not_assessed'::text])),
    CONSTRAINT maintenance_work_lanes_check
        CHECK ((cardinality(lanes) > 0) AND (lanes <@ ARRAY['changed'::text, 'aging'::text])),
    CONSTRAINT maintenance_work_outcome_check
        CHECK (outcome = ANY (ARRAY['completed'::text, 'inconclusive'::text, 'failed'::text,
                                    'superseded'::text])),
    CONSTRAINT maintenance_work_reported_tokens_check CHECK (reported_tokens >= 0),
    CONSTRAINT maintenance_work_pkey PRIMARY KEY (id),
    CONSTRAINT maintenance_work_catalog_id_id_key UNIQUE (catalog_id, id),
    CONSTRAINT maintenance_work_catalog_id_run_id_id_key UNIQUE (catalog_id, run_id, id),
    CONSTRAINT maintenance_work_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE,
    CONSTRAINT maintenance_work_catalog_id_path_revision_fkey FOREIGN KEY (catalog_id, path, revision)
        REFERENCES catalog.revisions (catalog_id, path, revision) DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT maintenance_work_catalog_id_run_id_fkey FOREIGN KEY (catalog_id, run_id)
        REFERENCES catalog.maintenance_runs (catalog_id, id) ON DELETE CASCADE
);
CREATE INDEX maintenance_work_lease_expiry
    ON catalog.maintenance_work USING btree (catalog_id, lease_expires_at)
    WHERE (outcome IS NULL);
CREATE UNIQUE INDEX maintenance_work_live_claim
    ON catalog.maintenance_work USING btree (catalog_id, path, revision)
    WHERE (outcome IS NULL);
CREATE INDEX maintenance_work_path_revision
    ON catalog.maintenance_work USING btree (catalog_id, path, revision);
CREATE INDEX maintenance_work_run ON catalog.maintenance_work USING btree (catalog_id, run_id);

CREATE TABLE catalog.maintenance_findings (
    id           text NOT NULL,
    run_id       text NOT NULL,
    work_id      text NOT NULL,
    catalog_id   bigint NOT NULL,
    category     text NOT NULL,
    assertion    text NOT NULL,
    follow_up    text NOT NULL,
    uncertainty  text NOT NULL,
    relationship text NOT NULL,
    external     jsonb DEFAULT '[]'::jsonb NOT NULL,
    created_at   timestamptz NOT NULL,
    CONSTRAINT maintenance_findings_category_check
        CHECK (category = ANY (ARRAY['suggested_link'::text, 'possible_duplicate'::text,
                                     'conflicting_assertions'::text,
                                     'potentially_outdated_information'::text,
                                     'unverifiable_claim'::text])),
    CONSTRAINT maintenance_findings_relationship_check
        CHECK (relationship = ANY (ARRAY['direct_statement'::text, 'inference'::text,
                                         'external_observation'::text])),
    CONSTRAINT maintenance_findings_pkey PRIMARY KEY (id),
    CONSTRAINT maintenance_findings_catalog_id_id_key UNIQUE (catalog_id, id),
    CONSTRAINT maintenance_findings_catalog_id_work_id_id_key UNIQUE (catalog_id, work_id, id),
    CONSTRAINT maintenance_findings_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE,
    CONSTRAINT maintenance_findings_catalog_id_run_id_work_id_fkey
        FOREIGN KEY (catalog_id, run_id, work_id)
        REFERENCES catalog.maintenance_work (catalog_id, run_id, id) ON DELETE CASCADE
);
CREATE INDEX maintenance_findings_catalog_created
    ON catalog.maintenance_findings USING btree (catalog_id, created_at, id);
CREATE INDEX maintenance_findings_run
    ON catalog.maintenance_findings USING btree (catalog_id, run_id);
CREATE INDEX maintenance_findings_work
    ON catalog.maintenance_findings USING btree (catalog_id, work_id);

CREATE TABLE catalog.maintenance_evidence (
    catalog_id bigint NOT NULL,
    work_id    text NOT NULL,
    finding_id text,
    ordinal    integer NOT NULL,
    path       text NOT NULL,
    revision   integer NOT NULL,
    excerpt    text NOT NULL,
    CONSTRAINT maintenance_evidence_ordinal_check CHECK (ordinal >= 0),
    CONSTRAINT maintenance_evidence_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE,
    CONSTRAINT maintenance_evidence_catalog_id_path_revision_fkey
        FOREIGN KEY (catalog_id, path, revision)
        REFERENCES catalog.revisions (catalog_id, path, revision) DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT maintenance_evidence_catalog_id_work_id_finding_id_fkey
        FOREIGN KEY (catalog_id, work_id, finding_id)
        REFERENCES catalog.maintenance_findings (catalog_id, work_id, id) ON DELETE CASCADE,
    CONSTRAINT maintenance_evidence_catalog_id_work_id_fkey FOREIGN KEY (catalog_id, work_id)
        REFERENCES catalog.maintenance_work (catalog_id, id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX maintenance_evidence_assessment_ordinal
    ON catalog.maintenance_evidence USING btree (catalog_id, work_id, ordinal)
    WHERE (finding_id IS NULL);
CREATE INDEX maintenance_evidence_finding
    ON catalog.maintenance_evidence USING btree (catalog_id, work_id, finding_id);
CREATE UNIQUE INDEX maintenance_evidence_finding_ordinal
    ON catalog.maintenance_evidence USING btree (catalog_id, work_id, finding_id, ordinal)
    WHERE (finding_id IS NOT NULL);
CREATE INDEX maintenance_evidence_revision
    ON catalog.maintenance_evidence USING btree (catalog_id, path, revision);

CREATE TABLE catalog.maintenance_dispositions (
    id            bigint GENERATED ALWAYS AS IDENTITY,
    catalog_id    bigint NOT NULL,
    finding_id    text NOT NULL,
    disposition   text NOT NULL,
    rationale     text NOT NULL,
    credential_id text NOT NULL,
    actor         text NOT NULL,
    created_at    timestamptz NOT NULL,
    CONSTRAINT maintenance_dispositions_disposition_check
        CHECK (disposition = ANY (ARRAY['open'::text, 'accepted_for_follow_up'::text,
                                        'dismissed'::text, 'needs_input'::text])),
    CONSTRAINT maintenance_dispositions_pkey PRIMARY KEY (id),
    CONSTRAINT maintenance_dispositions_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE,
    CONSTRAINT maintenance_dispositions_catalog_id_finding_id_fkey FOREIGN KEY (catalog_id, finding_id)
        REFERENCES catalog.maintenance_findings (catalog_id, id) ON DELETE CASCADE
);
CREATE INDEX maintenance_dispositions_finding
    ON catalog.maintenance_dispositions USING btree (catalog_id, finding_id, id);

CREATE TABLE catalog.maintenance_requests (
    catalog_id      bigint NOT NULL,
    credential_id   text NOT NULL,
    operation       text NOT NULL,
    idempotency_key text NOT NULL,
    request         jsonb NOT NULL,
    response        jsonb NOT NULL,
    CONSTRAINT maintenance_requests_pkey
        PRIMARY KEY (catalog_id, credential_id, operation, idempotency_key),
    CONSTRAINT maintenance_requests_catalog_id_fkey FOREIGN KEY (catalog_id)
        REFERENCES catalog.catalog_state (id) ON DELETE CASCADE
);

-- Catalog-event audit entries (spec Decision 10, plan P4). The identity
-- columns hold account-database ids as plain values; entries outlive their
-- catalog (P5), so `catalog_id` references nothing.
CREATE TABLE catalog.catalog_audit_entries (
    id                   bigint GENERATED ALWAYS AS IDENTITY,
    at                   timestamptz NOT NULL,
    catalog_id           bigint NOT NULL,
    action               text NOT NULL,
    source               text NOT NULL,
    actor_credential_ref bigint,
    actor_person_id      bigint,
    actor_label          text NOT NULL,
    actor_operator       boolean NOT NULL,
    client_ip            inet,
    target_name          text NOT NULL,
    target_person_id     bigint,
    target_operator      boolean DEFAULT false NOT NULL,
    teams                bigint[] DEFAULT '{}'::bigint[] NOT NULL,
    details              jsonb DEFAULT '{}'::jsonb NOT NULL,
    CONSTRAINT catalog_audit_entries_action_check CHECK (action ~ '^[a-z]+\.[a-z_]+$'::text),
    CONSTRAINT catalog_audit_entries_details_check CHECK (jsonb_typeof(details) = 'object'::text),
    CONSTRAINT catalog_audit_entries_source_check
        CHECK (source = ANY (ARRAY['cli'::text, 'api'::text, 'console'::text, 'system'::text])),
    CONSTRAINT catalog_audit_entries_pkey PRIMARY KEY (id)
);
CREATE INDEX catalog_audit_entries_at ON catalog.catalog_audit_entries USING btree (at, id);
CREATE INDEX catalog_audit_entries_teams ON catalog.catalog_audit_entries USING gin (teams);

-- Append-only, as the account table is. A store has no server_settings, so
-- the prune passes the retention period in the transaction-local setting
-- `okf.audit_retention_days`; without it (or with a value that is not a
-- number), 400 days. Never less than 30.
CREATE FUNCTION catalog.catalog_audit_entries_append_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
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
CREATE TRIGGER catalog_audit_entries_append_only
    BEFORE DELETE OR UPDATE ON catalog.catalog_audit_entries
    FOR EACH ROW EXECUTE FUNCTION catalog.catalog_audit_entries_append_only();
CREATE TRIGGER catalog_audit_entries_no_truncate
    BEFORE TRUNCATE ON catalog.catalog_audit_entries
    FOR EACH STATEMENT EXECUTE FUNCTION catalog.catalog_audit_entries_append_only();

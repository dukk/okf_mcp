-- One catalog.maintenance_run entry per maintenance runner pass (M14 spec
-- Part 6): written when a run leaves 'running', however it ends (closed by
-- the runner or an operator, completed, or failed by recovery), or when a
-- claim that found nothing to do inserts it already 'completed', in that
-- statement's own transaction. The runner's individual reads are not
-- recorded. The catalogs.maintenance flag itself is in 0016_unlocks.sql.
--
-- Lock order (tools/agentic/lessons.md): a run ends under its catalog's
-- locks, after users would have had to be locked, so this trigger locks no
-- user and waits for no audit row. It does not call audit_person,
-- audit_credential or audit_team_ref (which lock users FOR KEY SHARE, and
-- users::delete holds its user FOR UPDATE while it deletes the user's keys,
-- which sets maintenance_runs.key_id to NULL: a deadlock). It reads the
-- rows those resolvers keep: every key gets its audit_credentials row when
-- the key cache loads it (gates::ensure_audit_rows), and every team gets its
-- audit_teams row when it is created (0014_audit_row_locks.sql). The
-- credential row is taken FOR KEY SHARE SKIP LOCKED: the entry's foreign key
-- takes that lock anyway, and a row that forget_person is clearing (FOR
-- UPDATE, during the user's deletion) is skipped rather than waited for;
-- the entry then names no credential.
-- The entry counts the run's findings while the run's transaction holds
-- the catalog's locks; without this index that count would scan every
-- finding the catalog ever had (0006 indexes findings by creation time and
-- by work item, neither of which selects a run).
CREATE INDEX maintenance_findings_run ON maintenance_findings (catalog_id, run_id);

CREATE FUNCTION audit_maintenance_run() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    credential bigint;
BEGIN
    SELECT a.id INTO credential FROM audit_credentials a
     WHERE a.credential_id = NEW.credential_id
       FOR KEY SHARE SKIP LOCKED;
    -- No label in the entry (M14 Decision 17): it says 'system:maintenance'.
    -- An admin-level key, or a key of an admin user, is operator staff
    -- running a pass, which owners see as "an operator" (as audit::record
    -- marks it, issue #402).
    INSERT INTO audit_entries (at, action, source, actor_credential_ref, actor_label,
                               actor_operator, target_kind, target_name, teams, details)
    SELECT COALESCE(NEW.ended_at, NEW.deadline), 'catalog.maintenance_run', 'system',
           credential, 'system:maintenance',
           EXISTS (SELECT 1 FROM api_keys k LEFT JOIN users u ON u.id = k.user_id
                    WHERE k.id = NEW.key_id AND (k.access = 'admin' OR COALESCE(u.is_admin, false))),
           'catalog', c.name,
           COALESCE((SELECT ARRAY[t.id] FROM audit_teams t WHERE t.team_id = c.owner_team_id),
                    '{}'::bigint[]),
           jsonb_build_object(
               'run', NEW.id,
               'state', NEW.state,
               'findings', (SELECT count(*) FROM maintenance_findings f
                            WHERE f.catalog_id = NEW.catalog_id AND f.run_id = NEW.id))
    FROM catalogs c WHERE c.id = NEW.catalog_id;
    RETURN NEW;
END
$$;

-- Two triggers, because an INSERT trigger's WHEN cannot name OLD.
CREATE TRIGGER maintenance_runs_audit_end AFTER UPDATE OF state ON maintenance_runs
    FOR EACH ROW WHEN (OLD.state = 'running' AND NEW.state <> 'running')
    EXECUTE FUNCTION audit_maintenance_run();
CREATE TRIGGER maintenance_runs_audit_empty AFTER INSERT ON maintenance_runs
    FOR EACH ROW WHEN (NEW.state <> 'running')
    EXECUTE FUNCTION audit_maintenance_run();

-- Every user, team and live credential has its audit row (M18 spec
-- Decision 20). Until now gates::load created the missing ones on every
-- key-cache reload (gates::ensure_audit_rows): the credential row of every
-- live key and grant, the person of every user and the row of every
-- catalog's owner team. Per-request loading cannot write, so the store
-- functions that create a user, key, grant or team now make its row in
-- the transaction that creates it (users::create and the team writes have
-- done so since 0014_audit_row_locks.sql), and this backfills the rest:
-- in practice keys and grants from before the audit log that never acted.
--
-- The rows are what the resolvers in 0014_audit_row_locks.sql create, so
-- an entry that later names one only reads it:
--   * audit_person: the user's id, name and display name;
--   * audit_team_ref: a shared team's name, or a personal team's person;
--   * audit_credential(id, NULL, actor): the owner's person and the
--     credential's actor as its label, as audit::record passes it.
-- People first: teams and credentials point at them. Every team, not only
-- the catalog owners gates::load covered, as 0014 did. ON CONFLICT DO
-- NOTHING (and NOT EXISTS) keep a rerun, or a row an older replica made
-- meanwhile during a rolling deploy, from duplicating or touching a row.
-- No row is updated, so this takes no lock any resolver waits on.

INSERT INTO audit_people (user_id, name, display_name)
SELECT u.id, u.name, u.display_name FROM users u
WHERE NOT EXISTS (SELECT 1 FROM audit_people p WHERE p.user_id = u.id)
ON CONFLICT (user_id) DO NOTHING;

INSERT INTO audit_teams (team_id, name, person_id)
SELECT t.id,
       CASE WHEN t.personal_of IS NULL THEN t.name END,
       (SELECT p.id FROM audit_people p WHERE p.user_id = t.personal_of)
FROM teams t
WHERE NOT EXISTS (SELECT 1 FROM audit_teams a WHERE a.team_id = t.id)
ON CONFLICT (team_id) DO NOTHING;

-- A key's or grant's owner is api_keys.user_id (NULL for a legacy key with
-- no user, which then gets a row with no person, as audit_credential gives
-- it). Revoked credentials are left out, as gates::load left them.
INSERT INTO audit_credentials (credential_id, person_id, label)
SELECT k.id, (SELECT p.id FROM audit_people p WHERE p.user_id = k.user_id), k.actor
FROM api_keys k
WHERE k.revoked_at IS NULL
  AND NOT EXISTS (SELECT 1 FROM audit_credentials a WHERE a.credential_id = k.id)
ON CONFLICT (credential_id) DO NOTHING;

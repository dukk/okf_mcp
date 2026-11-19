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
-- Owners are locked first, as the resolvers lock them (audit_person takes
-- the user FOR KEY SHARE and skips one deleted meanwhile). During a rolling
-- deploy an old replica may be deleting a user: users::delete holds the
-- user FOR UPDATE, forgets its audit rows (forget_person), then deletes
-- it. Without the lock, this statement's snapshot still shows that user
-- and its keys, and would re-create the forgotten name or credential label
-- after the deletion commits. With it, the lock waits for the deletion and
-- the row is skipped; each later statement (a new snapshot, under READ
-- COMMITTED) no longer sees the user or its cascaded keys, and the user
-- stays locked until this migration commits. The order is the users'
-- lock order (users::set_admin, users::delete): admins by id, then the
-- others by id; keys come after users, as for every writer
-- (tools/agentic/lessons.md). FOR KEY SHARE conflicts only with FOR UPDATE
-- (a deletion, set_admin), not with the resolvers' own FOR KEY SHARE.
SELECT 1 FROM users ORDER BY is_admin DESC, id FOR KEY SHARE;

-- People first: teams and credentials point at them. Every team, not only
-- the catalog owners gates::load covered, as 0014 did. ON CONFLICT DO
-- NOTHING (and NOT EXISTS) keep a rerun, or a row an older replica made
-- meanwhile during a rolling deploy, from duplicating or touching a row.
-- No row is updated, so this takes no lock any resolver waits on.

INSERT INTO audit_people (user_id, name, display_name)
SELECT u.id, u.name, u.display_name FROM users u
WHERE NOT EXISTS (SELECT 1 FROM audit_people p WHERE p.user_id = u.id)
FOR KEY SHARE OF u
ON CONFLICT (user_id) DO NOTHING;

INSERT INTO audit_teams (team_id, name, person_id)
SELECT t.id,
       CASE WHEN t.personal_of IS NULL THEN t.name END,
       (SELECT p.id FROM audit_people p WHERE p.user_id = t.personal_of)
FROM teams t
WHERE NOT EXISTS (SELECT 1 FROM audit_teams a WHERE a.team_id = t.id)
ON CONFLICT (team_id) DO NOTHING;

-- A key's or grant's owner is api_keys.user_id. The owner is locked again
-- here (a no-op for one locked above), so one created since and deleted
-- meanwhile is waited for and skipped too. Revoked credentials are left
-- out, as gates::load left them.
INSERT INTO audit_credentials (credential_id, person_id, label)
SELECT k.id, p.id, k.actor
FROM api_keys k
JOIN users u ON u.id = k.user_id
JOIN audit_people p ON p.user_id = u.id
WHERE k.revoked_at IS NULL
  AND NOT EXISTS (SELECT 1 FROM audit_credentials a WHERE a.credential_id = k.id)
FOR KEY SHARE OF u
ON CONFLICT (credential_id) DO NOTHING;

-- A legacy key with no user gets a row with no person, as audit_credential
-- gives it. Its label can name a person, so the key is held FOR SHARE,
-- which waits for a concurrent keys::assign_user (an UPDATE): the row is
-- then read again and, now owned, skipped rather than left unlinked from
-- the person forget_person would clear it with.
INSERT INTO audit_credentials (credential_id, person_id, label)
SELECT k.id, NULL, k.actor
FROM api_keys k
WHERE k.user_id IS NULL AND k.revoked_at IS NULL
  AND NOT EXISTS (SELECT 1 FROM audit_credentials a WHERE a.credential_id = k.id)
FOR SHARE OF k
ON CONFLICT (credential_id) DO NOTHING;

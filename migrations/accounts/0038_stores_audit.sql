-- Audit entries about a store (`store.added`, ...): target_kind gains
-- 'store', whose target is its name and nothing else. Both new checks accept
-- everything the old ones did (no stored row has the new kind), so each is
-- added NOT VALID: the audit log is not scanned under the ACCESS EXCLUSIVE
-- lock the DROP and ADD take on `audit_entries` (a moment). 0039 validates
-- them. The shape CHECK's text is 0012's with one new arm. This file holds
-- only these statements, so the lock is not held while any other table is
-- waited for.
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

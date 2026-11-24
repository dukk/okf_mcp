-- Unlock before access (spec Part 6). An unlock lets one admin user's
-- credentials into one catalog's contents, for a reason, until it expires
-- or is ended; it is never extended. The tallies count what was done
-- under it. The per-catalog maintenance flag lets an owner keep the
-- maintenance runner out (default on).
CREATE TABLE unlocks (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    -- NULL once the user is deleted; person_id keeps who it was.
    user_id    text REFERENCES users (id) ON DELETE SET NULL,
    person_id  bigint NOT NULL REFERENCES audit_people (id),
    category   text NOT NULL CHECK (category IN ('support', 'deletion_request', 'export_request',
                                                 'security_investigation', 'legal', 'other')),
    reason     text NOT NULL CHECK (char_length(btrim(reason)) BETWEEN 1 AND 500),
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL CHECK (expires_at > created_at),
    ended_at   timestamptz CHECK (ended_at IS NULL OR ended_at >= created_at),
    reads      bigint NOT NULL DEFAULT 0 CHECK (reads >= 0),
    writes     bigint NOT NULL DEFAULT 0 CHECK (writes >= 0),
    exports    bigint NOT NULL DEFAULT 0 CHECK (exports >= 0)
);
-- The key cache loads the live ones at every reload.
CREATE INDEX unlocks_live ON unlocks (expires_at) WHERE ended_at IS NULL;
CREATE INDEX unlocks_catalog ON unlocks (catalog_id, created_at DESC);
-- The `ON DELETE SET NULL` of a user's deletion finds its rows by this.
CREATE INDEX unlocks_user_id ON unlocks (user_id) WHERE user_id IS NOT NULL;

ALTER TABLE catalogs ADD COLUMN maintenance boolean NOT NULL DEFAULT true;

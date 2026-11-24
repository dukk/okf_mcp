-- Server-side keys, one row per purpose (issue #430). The first is
-- `audit_cursor`: the key that seals a team owner's audit cursor, so the
-- owner view never shows a value of the global `audit_entries.id`
-- sequence (`store::server_keys`, `http::api::audit::cursor`). The server
-- creates a row the first time it needs one (random bytes from the OS,
-- `INSERT ... ON CONFLICT DO NOTHING`), so every replica reads the same key
-- and it survives restarts. Deleting a row rotates that key: the next use
-- creates a new one.
CREATE TABLE server_keys (
    name       text PRIMARY KEY,
    key        bytea NOT NULL CHECK (octet_length(key) = 32),
    created_at timestamptz NOT NULL DEFAULT now()
);

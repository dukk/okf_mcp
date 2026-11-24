-- Which store a database belongs to (catalog stores plan, Task 14).
--
-- One row: the name of the `stores` row the database answers to. Migrating
-- or adopting a store writes it if it is absent (`main` for the account
-- database), and every later open compares it with the store being opened.
-- So one physical database answers to one store name, whatever host name
-- reached it: a second `stores` row for the same database under another host
-- name, or the account database added as a `host` store, is refused.
--
-- A migration of its own, not part of the baseline: the account database
-- adopts the baseline (records it without running it) and then runs this
-- file, so adopted and new stores both get the table the same way.
CREATE TABLE catalog.store_identity (
    name text NOT NULL,
    CONSTRAINT store_identity_pkey PRIMARY KEY (name)
);
-- At most one row: every row has the same value for `(true)`.
CREATE UNIQUE INDEX store_identity_one_row ON catalog.store_identity ((true));

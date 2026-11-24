-- Provisioner auditing (issue #423): `user.looked_up` and
-- `provisioner.refused` entries. A lookup that matches no user, and every
-- refusal, are about a route rather than a user, a team or a catalog, so an
-- entry may now have the target kind 'route' (its target_name is
-- "METHOD template"; the existing target-shape check already requires a
-- name for any kind it does not list).
--
-- The new set is a superset of the old one, so every stored row already
-- satisfies it: the constraint is added NOT VALID, which enforces it for
-- new rows without a scan of the whole log under the table lock.
-- `0032_provisioner_audit_validate.sql` validates it in its own transaction.
ALTER TABLE audit_entries DROP CONSTRAINT audit_entries_target_kind_check;
ALTER TABLE audit_entries ADD CONSTRAINT audit_entries_target_kind_check CHECK (
    target_kind IN ('user', 'team', 'catalog', 'key', 'grant', 'code', 'setting', 'unlock', 'route')
) NOT VALID;

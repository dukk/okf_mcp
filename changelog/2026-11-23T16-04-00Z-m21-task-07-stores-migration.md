# M21 Task 7: migration 0036 (stores, placement, registry columns) and the folder move

## Added
- `migrations/accounts/0036_stores.sql`:
  - `stores` (one row per database catalog data can live in): `name`, `location_kind` (`account-database` or `host`), `host`, `port`, `database`, `credential_kind` (`iam`, `secrets-manager`, `env`), `credential_ref`, `created_at`. Two table CHECKs tie the kind to the columns: an account-database row has none of host, port, database, credential kind or reference; a host row has all five. The table holds a reference to a credential, never a credential.
  - A `main` row (`account-database`), the only one allowed (`stores_one_account_database`), and one row per `lower(host)`, port and database (`stores_one_row_per_database`).
  - The `default_store` setting, `"main"`, written `ON CONFLICT DO NOTHING`.
  - `teams.store` (nullable, `REFERENCES stores ON DELETE RESTRICT`): a team's pin.
  - `catalogs.store` (`NOT NULL DEFAULT 'main'`, `ON DELETE RESTRICT`) and `catalogs.provisioned_at` (`DEFAULT now()`, set to `created_at` for every existing row), and the partial index `catalogs_unprovisioned` on `created_at` where `provisioned_at IS NULL`.
  - Audit target kind `store`: `audit_entries_target_kind_check` lists it, and `audit_entries_target_shape` has a `WHEN 'store'` arm (a name, and no team, credential or person reference). The shape CHECK's text is 0012's with that one arm added.
- `migrations/accounts/0037_stores_audit_validate.sql` validates the two audit CHECKs in a transaction of its own.
- `tests/fixtures/migration_history_0035.txt`: the version, description and SHA-384 checksum of each of migrations 0001-0035, taken before the move.
- Tests in `src/db.rs`: `moving_the_migrations_folder_keeps_their_history`, `migration_0036_puts_every_catalog_on_main_provisioned`, `a_release_one_create_after_0036_lands_on_main_provisioned`, `stores_refuses_a_host_row_without_a_credential_reference`, `stores_refuses_an_account_database_row_with_a_host`, `a_second_account_database_store_is_refused`, `a_second_row_for_the_same_host_port_and_database_is_refused`, `a_store_cannot_be_removed_while_a_catalog_or_team_uses_it`, `audit_entries_accept_a_store_target_by_name_only`.

## Changed
- `migrations/*.sql` moved to `migrations/accounts/` with `git mv`; no file changed. `src/db.rs` and `tests/support/pg.rs` embed `./migrations/accounts`; `include_str!` paths in `src/db.rs` tests and the directory scan in `tests/telemetry_privacy.rs` follow. `build.rs` watches the whole `migrations` directory, which includes the subfolder, so only its comment changed. `Dockerfile.maintenance` and `scripts/test_maintenance_image.py` copy `migrations` as a whole and needed no change. Doc comments and the README name the new folder.
- The test template's name hashes each migration's version and checksum, so it changes only because 0036 and 0037 are new.
- The plan: later migrations move up one (Task 11 is now 0038, Task 12 is 0039), with a `Corrected (issue #965)` note at Task 7.

## Notes
- **The move changes no history.** sqlx records and compares a migration's version, description and checksum (SHA-384 of the file text), none of which involve the folder. The test migrates a database to 0035, checks its `_sqlx_migrations` rows against the fixture taken before the move, runs the server's `migrate`, and checks the rows again; a mismatch would make `migrate` fail with a version mismatch.
- **Locks, statement by statement.** sqlx runs the file in one transaction, so each lock lasts until it commits.
  - `CREATE TABLE stores`, its inserts and indexes: a new table, no waiters.
  - `ALTER TABLE teams ADD COLUMN store text REFERENCES stores`: ACCESS EXCLUSIVE on `teams` for an instant. A nullable column with no default is metadata-only, and the foreign key check finds only NULLs.
  - `ALTER TABLE catalogs ADD COLUMN ... DEFAULT 'main'` and `... DEFAULT now()`: ACCESS EXCLUSIVE on `catalogs`, metadata-only. Since PostgreSQL 11 a default that is not volatile is stored in the catalog and no row is rewritten; `now()` is STABLE, so it qualifies (the default's value is fixed at the ALTER). The foreign key checks the single value `'main'`.
  - `UPDATE catalogs SET provisioned_at = created_at` rewrites each registry row once (one row per catalog), in the same transaction as the ALTER. `teams` is locked before `catalogs`, the order every writer uses (`tools/agentic/lessons.md`).
  - `CREATE INDEX catalogs_unprovisioned`: a plain build is correct here because the index is partial and, right after the UPDATE, matches no row; it adds nothing to the lock already held.
  - The two audit CHECKs are re-added `NOT VALID`, which takes ACCESS EXCLUSIVE on `audit_entries` for an instant and skips the scan of the whole log; 0037 validates them under SHARE UPDATE EXCLUSIVE, so the log stays readable and writable (the pattern of 0031 and 0032). This is why the plan's 0037 and 0038 became 0038 and 0039.
- **Rolling deploy (P11).** A release-1 replica's `INSERT INTO catalogs` names neither new column and so lands on `main`, provisioned; `a_release_one_create_after_0036_lands_on_main_provisioned` runs that statement. An older replica ignores the new columns on teams and catalogs, and writes no `store` audit entry.
- Nothing reads `stores`, `teams.store` or `catalogs.store` yet; Task 8 adds the registry in code.

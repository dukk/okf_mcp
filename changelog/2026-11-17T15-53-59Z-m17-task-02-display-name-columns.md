# M17 Task 2: migration 0019 and the display name on rows

## Added
- `migrations/0019_display_names.sql`: `catalogs.display_name` (`text NOT NULL DEFAULT ''`); existing user and team display names normalised the way `DisplayName::parse` reads them (control characters and U+2028/U+2029 to spaces, Unicode whitespace trimmed, the first 80 characters kept, trimmed again); a `char_length(display_name) <= 80` CHECK on `users`, `teams` and `catalogs`.
- Tests: `POST /users` and `/catalogs` store a trimmed display name, and `POST /users`, `/teams` and `/catalogs` refuse a bad one with a 422 that writes nothing; the CLI's `user create` and `team create` do the same (refusing 81 characters); `catalogs::create` stores the display name and `find` and `list` read it; the CHECK refuses 81 characters on all three tables (80 two-byte characters fit); `UserAuth` carries the display name (`find_by_email`, `auth_by_id`); a new user's personal team gets its display name; the migration from 0018 over seeded legacy rows (exact normalised values, clean rows not rewritten, the CHECKs in force).

## Changed
- `Catalog` and `UserAuth` gain `display_name`; `select_catalogs!`, the catalog `INSERT .. RETURNING` and `user_auth_columns!` read it.
- `NewCatalog`, `NewUser` and `NewTeam` take `display_name: &DisplayName`, so no create path can store an unchecked name.
- `POST /users`, `POST /teams` and `POST /catalogs` (which gains an optional `display_name`), and the CLI's `user create` and `team create`, parse the name with `DisplayName::parse` first: a refused name is a 422 (`limit: "display_name"`) that writes nothing.
- `docs/operations.md`: the `POST /api/v1/users`, `/teams` and `/catalogs` rows name the display-name rule and its 422.
- The M17 plan's Task 2 SQL and tests are corrected (`> **Corrected (issue #599).**`).

## Notes
- Additive: an older replica ignores the new catalog column (its inserts get the default). Its user and team creates do not check the length, so during a rolling upgrade one given more than 80 characters fails the CHECK (a 500) instead of storing it.
- The CLI's `catalog create --display-name` is Task 6; it passes an empty name until then.

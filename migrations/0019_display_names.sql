-- Display names (spec 2026-11-17-okf-display-names-design.md). A slug
-- (`name`) stays the permanent id; `display_name` is the human label, empty
-- meaning unset. Catalogs gain the column. Users and teams have had it since
-- 0009, never validated, so existing values are repaired until
-- `labels::DisplayName::parse` accepts each one unchanged (where `parse`
-- refuses a bad name, this rewrites it) before the CHECK goes on:
--   1. every control character (Unicode Cc, U+0001-U+001F and U+007F-U+009F,
--      what Rust's `char::is_control` matches) and U+2028/U+2029 becomes a
--      space;
--   2. leading and trailing whitespace goes (Unicode White_Space, what
--      Rust's `str::trim` removes, not only ASCII spaces as `btrim` would);
--   3. the first 80 characters stay, and whitespace the cut leaves at the
--      end goes too.
-- Explicit code-point ranges rather than `[[:cntrl:]]` and `[[:space:]]`:
-- those depend on the database's locale. With `standard_conforming_strings`
-- on (the default), `\uXXXX` reaches the regex engine as written, and the
-- ARE syntax reads it as that code point. Only rows the rule changes are
-- rewritten. Additive: an older replica ignores the new column.
--
-- Locks. sqlx runs a migration in one transaction, so every lock below is
-- held until it commits; the statements are ordered to take the blocking
-- ones last. The cleanup `UPDATE`s come first and take only ROW EXCLUSIVE
-- (other readers and writers carry on; only a changed row is locked). Each
-- `ADD CONSTRAINT` then takes ACCESS EXCLUSIVE on its table for the rest of
-- the transaction, so `catalogs` (which every MCP and REST request reads) is
-- locked last, by one `ALTER` whose column add is metadata only (a constant
-- default needs no rewrite) and whose CHECK scans the catalog rows. From the
-- first exclusive lock to the commit, the migration does three scans of
-- tables holding one row per user, team or catalog.

UPDATE users u SET display_name = n.display_name
FROM (
    SELECT id, regexp_replace(
               left(regexp_replace(
                        regexp_replace(display_name,
                                       '[\u0001-\u001f\u007f-\u009f\u2028\u2029]', ' ', 'g'),
                        '^[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+'
                        '|[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+$',
                        '', 'g'),
                    80),
               '[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+$',
               '') AS display_name
    FROM users
) n
WHERE u.id = n.id AND u.display_name <> n.display_name;

UPDATE teams t SET display_name = n.display_name
FROM (
    SELECT id, regexp_replace(
               left(regexp_replace(
                        regexp_replace(display_name,
                                       '[\u0001-\u001f\u007f-\u009f\u2028\u2029]', ' ', 'g'),
                        '^[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+'
                        '|[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+$',
                        '', 'g'),
                    80),
               '[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+$',
               '') AS display_name
    FROM teams
) n
WHERE t.id = n.id AND t.display_name <> n.display_name;

ALTER TABLE users ADD CONSTRAINT users_display_name_length CHECK (char_length(display_name) <= 80);
ALTER TABLE teams ADD CONSTRAINT teams_display_name_length CHECK (char_length(display_name) <= 80);

ALTER TABLE catalogs
    ADD COLUMN display_name text NOT NULL DEFAULT '',
    ADD CONSTRAINT catalogs_display_name_length CHECK (char_length(display_name) <= 80);

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
-- held until it commits.
--   * `LOCK TABLE users, teams IN SHARE ROW EXCLUSIVE MODE` comes first. It
--     blocks inserts, updates and deletes on both tables (an older replica's
--     user or team create waits), but not reads or `FOR SHARE`/`FOR UPDATE`
--     row locks. Without it, a create from an older replica committed after
--     a cleanup `UPDATE` took its snapshot would be neither cleaned nor
--     refused: an 81-character name then fails `ADD CONSTRAINT` and rolls
--     the whole migration back (the new replica cannot start), and a short
--     name holding a control character survives unrepaired. A create that
--     waited runs after the commit, against the CHECK. (An older replica
--     still checks nothing itself, so until every replica is upgraded it
--     can store a short name with a control character; only the length is
--     enforced in SQL.) `users` comes first, the order `users::create`
--     writes them in. Should an older writer hold `teams` and then want
--     `users`, Postgres detects the deadlock and aborts one side; if that
--     is the migration, the new replica fails to start until restarted.
--   * The cleanup `UPDATE`s then run under that lock.
--   * Each `ADD CONSTRAINT` takes ACCESS EXCLUSIVE on its table (reads wait
--     too) until the commit, so they come last, and `catalogs` (which every
--     MCP and REST request reads) is locked last of all, by one `ALTER`
--     whose column add is metadata only (a constant default needs no
--     rewrite) and whose CHECK scans the catalog rows.
-- The tradeoff: user and team writes pause for the whole migration (two
-- cleanup scans, three CHECK scans), and their reads for the CHECK scans;
-- catalog reads and writes pause only for the last `ALTER` and the commit.
-- Every scanned table holds one row per user, team or catalog.

LOCK TABLE users, teams IN SHARE ROW EXCLUSIVE MODE;

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

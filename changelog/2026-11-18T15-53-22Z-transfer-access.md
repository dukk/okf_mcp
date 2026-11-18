# Import with write access; Import & export in the main navigation (#260)

## Added
- `a_writer_imports_and_a_reader_changes_nothing` (`src/http/archives.rs`): a
  `write` key imports through `/catalogs/{name}/import` and the next read sees
  it; a `read` key's valid archive is refused (403) and writes nothing.
- `a_member_imports_where_they_write_and_only_exports_where_they_read`
  (`tests/ui_e2e/catalogs.rs`): a member session entitled to `main` at
  `write` and `other` at `read` opens Import & export from the navigation,
  sees only those two catalogs, gets the import form only on `main`, exports
  `other` and imports into `main`.
- `store.canOn(level, catalog)` and `store.levelOn(catalog)` in the console,
  so a page can check a level on a catalog other than the selected one.

## Changed
- `import_catalog()` (both `/catalogs/{name}/import` and
  `/api/v1/catalogs/{catalog}/import`) now calls
  `principal.require(Access::Write, ..)` instead of `Access::Admin`, still
  before the upload is read. Reach is unchanged: an ungranted or unknown
  catalog is 404. A non-admin writer is refused (`Suspended`) on a suspended
  catalog, as for every other write; an admin is still exempt.
- The console's Import & export link moved out of the `data-need="admin"`
  Administer group into the main navigation. The rest of Administer
  (Catalogs, API keys, Users, Teams, Settings) is unchanged.
- The transfer page shows its import panel for `write` or more on the picked
  catalog (it was `store.can('admin')` on the selected one), and follows the
  picker.
- Tests: the `/api/v1` access matrix expects a writer's import to get past
  the key check (400 for a non-gzip body); the key-before-body test covers a
  reader (403), a writer outside its grants and an unknown catalog (404), and
  a writer and an admin (413); the suspension test expects `Suspended` for a
  writer and `Forbidden` for a reader; the session archive test, the server
  test and the console import journey now import as a writer.
- `Access` doc comments, `docs/operations.md` (route tables, suspension note,
  console description, troubleshooting) and `README.md` say import needs
  `write`.

## Notes
- The CLI's `okf-mcp import` talks to the database directly and never went
  through `import_catalog()`, so it is unaffected. No MCP tool imports.
- Import, export, replacement, generation, size-limit, timeout and audit
  semantics are unchanged.

# Issue #647: Import & export disables import for non-admins on a suspended catalog

## Changed

- `src/http/ui/js/pages/transfer.js`: when the picked catalog's `suspended`
  field (`{team, notice}`, from the `/catalogs` list the page already loads) is
  set and the caller is not an admin on it (`store.canOn('admin', ...)`), the
  Import panel stays visible but its controls are disabled (one
  `<fieldset disabled>`, and the button) and a note says the catalog is
  suspended and only an admin can import. The server refuses that import with
  403 `Suspended`, so the user no longer picks a file and uploads it to learn that.
  The state follows the picker and is refreshed after an import.
- Admins on a suspended catalog keep the working panel.
- `src/http/ui/styles.css`: `.import-controls` (a borderless fieldset).
- README and `docs/operations.md` describe the behaviour.

## Findings

- Export is the only other control on the page. The server allows export for
  a suspended catalog (`docs/operations.md`, team suspension), so it needs no change.

## Tests

- `a_suspended_catalog_disables_import_for_a_writer_but_not_an_admin`
  (`tests/ui_e2e/catalogs.rs`).

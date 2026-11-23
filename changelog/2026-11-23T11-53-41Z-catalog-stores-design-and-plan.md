# Catalog stores: design and implementation plan

## Added

- `docs/superpowers/specs/2026-11-23-okf-catalog-stores-design.md`: catalog data
  (documents, history, changesets, maintenance, catalog audit) can live in a
  database apart from the account database. Stores are rows in the account
  database; a new catalog lands on its team's pinned store or the current default;
  every catalog read and write is routed to its store. Decisions 1-18.
- `docs/superpowers/plans/2026-11-23-okf-mcp-catalog-stores.md`: 21 tasks in three
  deployable releases (types only; registry and placement; the schema split), then
  per-store housekeeping, cache signalling, reads across stores, docs and an
  end-to-end test.

## Notes

- Docs only; no code or schema change.
- Moving an existing catalog between stores is out of scope; it depends on the
  revision storage epic (#954).

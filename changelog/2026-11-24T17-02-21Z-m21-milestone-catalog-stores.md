# Milestone 21: catalog stores (epic #958)

## What the milestone delivers

Catalog data (documents, revisions, changesets, maintenance, catalog audit entries) can live in a store of its own: schema `catalog` in the account database (`main`, every existing deployment), or a separate database added with `okf-mcp stores add` or `POST /api/v1/stores`. A team can be pinned to a store, a default store takes new catalogs, and a store that is lost costs only its own catalogs (`StoreUnavailable`; merging reads name them in `unavailable`). Release 3 moves the catalog tables out of `public` in a maintenance window.

## Tasks and pull requests

| Task | Issue | PR |
|------|-------|----|
| 1 handle types | #959 | #981 |
| 2 writes take `CatalogDb` | #960 | #983 |
| 3 reads take `CatalogDb` | #961 | #984 |
| 4 maintenance | #962 | #985 |
| 5 split the joins | #963 | #987 |
| 6 every caller picks a side | #964 | #990 |
| 7 migration 0036 and folder move | #965 | #992 |
| 8 `Stores` registry | #966 | #994 |
| 9 `secrets-manager` and `iam` credentials | #967 | #996 |
| 10 placement and the admin surface | #968 | #997 |
| 10b two-step create, unprovisioned hidden | #969 | #1000 (part 1), #999 (part 2) |
| 11 schema `catalog`, `catalog_state` | #970 | #1001 |
| 12 catalog audit on the store | #971 | #1003 |
| 13 two-step delete, cleanup | #972 | #1005 |
| 14 migrators, baseline, adoption | #973 | #1007 |
| 15 suite on a separate store | #974 | #1008 |
| 16 per-store housekeeping | #976 | #1014 |
| 17 cache signalling per store | #975 | #1010 |
| 18 reads across stores | #977 | #1013 |
| 19 docs | #978 | #1018 |
| 20 end-to-end, smoke, P8 lifted | #979 | #1019 |

Also merged on the milestone branch: #991 and #1016 (syncs of `main`). The PR numbers come from `gh pr list --base milestone/958-catalog-stores --state merged`.

## Open follow-ups

- #993: `lock_timeout` on registry migrations; index the store columns before store queries.
- #995: store reads through `CatalogDb::executor()` surface pool timeouts as `Database`, not `StoreUnavailable`.
- #998: retry the acquisition once on an open store pool after a refused login.
- #1002: credential rotation tests fail intermittently with 28P01 under load.
- #1004: `an_absent_name_is_forgotten_after_its_lifetime` is timing-sensitive under load.
- #1017: two paths re-probe a store already found down in the same call.

## Before release 3

Follow "Upgrading to the release with separate stores" in `docs/operations.md`: snapshot, scale `serve` to zero, `okf-mcp migrate`, `okf-mcp stores migrate`, start. The automated smoke test, `the_upgrade_procedure_adopts_a_pre_split_database_and_keeps_its_history` in `tests/stores.rs` (described in `changelog/2026-11-24T17-02-20Z-m21-task-20-stores-end-to-end.md`), covers the procedure on a database built at the old schema; a run on a real snapshot is still to be done by the maintainer.

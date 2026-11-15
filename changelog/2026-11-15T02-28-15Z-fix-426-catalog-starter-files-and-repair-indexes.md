# Fix #426: new catalogs get starter files; `repair_indexes` repairs old ones

## Added

- `okf::ops::starter_files`: the root `index.md` (an `okf_version: "0.2"`
  frontmatter, the catalog's name as its heading, its description or a TODO
  line as the first paragraph) and `log.md` (`# Knowledge Catalog Update Log`
  with one `Creation` entry under the creation date) a new catalog starts
  with, after `okf-catalog-template`'s `install/skeleton`, without its people
  and projects folders.
- `okf::ops::repair_indexes` and the MCP tool `repair_indexes` (write
  access, staged in the changeset it names): creates a missing root
  `index.md` and `log.md`, an `index.md` for every folder holding a concept
  or an index (and their ancestors), lists every folder index in its
  parent's `# Sections` and every concept in its folder's index (its `title`,
  else first `# ` heading, else file name; its `description`, else a TODO
  placeholder). It only adds, skips machinery, and a second run changes
  nothing. Each created file and placeholder gets a note in the result.
- `ReplayOp::CreateLog`, and `ReplayOp::CreateIndex { folder: "" }` for the
  root: both keep whatever file exists at publish, so a repair published
  after someone else's curated root or log adds its listings to that text
  instead of overwriting it.
- Test helpers `mcp::testing::app_with_new` (catalogs created, nothing
  imported) and `store::catalogs::tests::drop_starter_files` (an empty
  catalog, as one created before this change is).

## Changed

- `store::catalogs::create` writes the starter files in the creating
  transaction: revision 1 of each, `create` revisions with the message
  `create catalog`. The catalog's generation stays 0.
- `store::import::import` treats a catalog at generation 0 (no publish or
  import yet) as empty: no `replace` needed. The import makes the catalog
  exactly the imported files, so a starter file the archive has is
  `updated`, one it lacks is `deleted`, and both continue their revision
  numbers.
- `POST /api/v1/catalogs` reports `files: 2` and the creation time as
  `last_change` instead of hard-coded `0` and `null`.
- `create_concept`'s refusal in a catalog without a root `index.md` now
  points at `repair_indexes`. `index`'s refusal keeps `okf.py`'s wording
  (pinned by `tests/golden/ops.json`); its parameter docs name the tool.
- `ensure_index`'s parent listing moved into `insert_folder_listing`, shared
  with `repair_indexes`.
- The console's New catalog dialog, `catalog create`'s help and the README
  and operations guide say a new catalog holds starter files, and describe
  `repair_indexes`.

## Notes

- Why not drop the starter files on the first import (so it reports exactly
  as before): the import's advisory lock does not exclude changeset writes,
  which hold `FOR KEY SHARE` on the catalog row. Dropping the rows would
  reuse revision 1, and a changeset based on a starter file could then
  publish over the import without a conflict. Continuing the revision
  numbers keeps the publish-time conflict check.
- Tests that exercise import mechanics, and the maintenance fixtures that
  insert their own revisions, now start from an explicitly empty catalog
  (`drop_starter_files`); `an_import_into_a_new_catalog_replaces_its_starter_files`
  covers the new path.
- The maintenance runner stays report-only. Automatic repair from it would
  need a separate explicit opt-in write feature; not built here.

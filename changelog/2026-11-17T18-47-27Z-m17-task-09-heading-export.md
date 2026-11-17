# M17 Task 9: the new catalog's heading and the export filename

## Added
- Tests: a new catalog's root `index.md` heading with and without a display name (`src/store/catalogs.rs`), and the export `Content-Disposition` with a label and with a label of no ASCII letters (`src/http/archives.rs`).

## Changed
- `starter_files` takes the root index's `heading` as its first argument: the catalog's display name, else `ROOT_HEADING` ("Catalog"). `write_starter_files` chooses it. `ROOT_HEADING` is now `pub(crate)`. Existing catalogs keep their headings.
- The export filename is `<export_stem(label)>-<slug>.tar.gz`, or `<slug>.tar.gz` when the stem is empty. The stem is ASCII letters, digits and `-`, so the header stays safe.
- The spec's "Catalog content and exports" section is corrected (Decision 3): the heading is the display name, not the label, because a label falls back to the description, which is already the paragraph beneath the heading.

## Notes
- The export label is the catalog's label from the key cache's directory, so a catalog with no display name but a description is named after the description, and one with neither is named `untitled-catalog-<slug>`.

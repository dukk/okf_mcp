# M17 Task 10c: labels across the remaining console pages (issue #609)

The console pages after the core show labels, and URLs keep slugs.

- Browse: the suspended view names the catalog and its team by label; the locked view names the catalog.
- Health, Runs: the catalog is named by its label. The run view has no `actor_label`, so the actor stays as is.
- Changesets: the catalog column is `named(catalog_label, catalog)`, the owner column shows `actor_label ?? actor`, and the drawer and the discard dialog use the labels. No Rust change: `GET /changesets` already returns `ListedChangeset` with both fields.
- Transfer: the catalog picker shows labels (values are slugs). The export link takes its download name from the server's `Content-Disposition` (`<stem>-<slug>.tar.gz`, since #606), falling back to `<slug>.tar.gz`.
- Unlocks: catalog and user cells use `named`; the End dialog and toast use labels.
- Keys, my-keys, my-grants: users, catalog lists, defaults and grants use `userLabel` and `catalogLabel`; `components/levels.js` labels its catalogs. Controls keep slugs as values and in `data-` attributes.

Tests (browser): the suspended view's labels, the export filename and picker label (`tests/ui_e2e/browse.rs`), changesets' catalog and actor labels (`changesets.rs`), the keys page's user and catalog labels (`keys.rs`). `TestServer::set_catalog_label` and `set_user_label` set a display name and reload the directory; `browse.rs` also tests the `exportFilename` fallback.

The plan's Task 10c carries a dated correction note.

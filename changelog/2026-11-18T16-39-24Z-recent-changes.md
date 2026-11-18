# Recent changes in the web console (#261)

## Added
- `GET /api/v1/catalogs/{catalog}/recent-changes?limit=&after=`
  (`src/http/api/browse.rs`): one page of the revisions published in a
  catalog, every document's, newest first, with `path`, `revision`, `op`,
  `actor`, `actor_label`, `message` and `at` (no content), and `next_after`
  for the next page. Revisions that share a time come by path, then revision,
  both descending, so the order is total. `limit` is 1..=200 (default 50;
  outside is 409, as for maintenance runs); `after` is `<revision>:<path>` of
  the previous page's last row (unknown or malformed is 404). It checks
  access as the revision route does (`require_contents`, then the catalog's
  selection and admission), so it refuses exactly as the history and
  revision routes do.
- `history::recent` (`src/store/history.rs`): the bounded catalog-wide query
  behind it, one statement for the newest page and one that continues after a
  cursor row with a row comparison.
- Migration `0020_revisions_recent_index.sql`: the index
  `revisions_catalog_recent` on `revisions (catalog_id, created_at, path,
  revision)`, which a backward scan reads in exactly the page's order.
- The console page Recent changes (`#/recent`, `js/pages/recent.js`), linked
  in the main navigation after Browse: the selected catalog's changes, with
  Refresh, Load older, and loading, empty and error states. A row opens the
  revision's diff and text in the drawer, a deleted document's too.
- `js/components/revision.js`: the revision drawer, taken out of
  `browse.js`, which now uses it.
- Tests: store tests for catalog isolation, every op, repeated revisions of
  one path, timestamp ties across pages of every size from 1 to 6, page size
  and cursor refusals, and the query plan (an index scan, no sort, no
  sequential scan); API tests for the published feed (no draft, nothing of
  another catalog, a deletion that opens through the revision route), cursor
  paging, parameter refusals, the access matrix and suspended catalogs, and
  that the route answers exactly as the history and revision routes do
  without a credential, for an unreached, a missing, a suspended and a
  locked catalog; browser journeys in `tests/ui_e2e/recent.rs`.

## Changed
- The revision drawer takes a claim on the drawer (`drawerTicket`) before
  its read, so of two revisions opened in turn the later one stays, whichever
  answer comes last. It used to check only that the page was still shown. It
  names the actor by label, with the id, when the directory knows one.
- `docs/operations.md` (console, API table, endpoint count) and `README.md`
  describe the page and the route.

## Notes
- Only published history is read (`revisions`); open changesets, maintenance
  activity and audit events are not part of the feed, and nothing updates
  live: Refresh reads again.
- The index is built inside the migration's transaction, as 0018's is, so
  writes to `revisions` wait while it builds once at upgrade.
- Revisions are only ever added (a catalog's go only with it), so a cursor
  row stays valid and paging an unchanged history neither repeats nor skips.

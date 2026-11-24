# Runs page pages newest first (issue #236)

## Added
- An opt-in `order` parameter (`asc` or `desc`) on `maintenance_list_runs` and
  `GET /api/v1/catalogs/{catalog}/maintenance/runs`. `desc` lists newest first;
  `after` (the previous page's `next_after`) continues in the chosen order.
  An unknown value is a 400 on the endpoint.
- A "Load older" button on the console's Runs page.
- Tests: a store test for the descending order and cursor (including unequal
  timestamps), API checks for `order`, and two browser journeys: the Runs page
  shows the newest run first and loads older ones on demand, and a failed
  "Load older" keeps the rows and the cursor so a retry adds the page once.

## Changed
- The Runs page loads one page of 20 runs, newest first, instead of following
  `next_after` through every page of runs and reversing the list.
- `docs/reference/tools.md` (regenerated) and `docs/operations.md` describe the
  new parameter.

## Notes
- The default order is unchanged (oldest first), so MCP clients and existing
  REST callers see the same results as before.
- The page size is a module variable in `pages/runs.js` with a
  `setRunsPageSize` export, so the journey can use a page of two instead of
  creating twenty-one runs.
- `Listing::Runs` now carries a `RunOrder`; the findings listing is untouched.

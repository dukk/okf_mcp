# M19 Task 05: the reader's client modules

## Added
- `src/http/read/js/api.js`: the reader's read client. `read` and `readAll`
  (following `next` cursors, stopping at one that does not move) answer
  `{ data, offline, cachedAt }` from the worker's `X-Read-Offline` and
  `X-Read-Cached-At`. `signOut` deletes the session with its CSRF token.
  `classify` maps every refusal to a `KIND` as the reader design's error
  table says; anything else is `failed`, never a revocation.
- `src/http/read/js/cache.js`: the page's side of the caches. `wipe`,
  `evictCatalog` and `evictDocument` first mark the scope in the worker
  (`'*'`, the encoded catalog, or the document's API path, the strings
  `sw.js` matches) and wait for its reply, then delete. `offlineDocuments`
  lists only the recorded owner's copies, newest first, one per path, and
  nothing without an owner record. Storage that is missing or refuses never
  fails any of them.
- `src/http/read/js/route.js` (hash routes, `hrefFor` with the `DOCUMENTS`
  rule, `fromBrowse` for links `src/render.rs` writes), `listing.js`
  (`folderListing`) and `parts.js` (search form, breadcrumbs, `renderBody`,
  notices).
- Node tests under `tests/read_worker/`, run by `just test-worker`:
  `route.test.mjs` (routes and listing), `api.test.mjs` (`classify`, reads,
  sign-out, with a fake `fetch`) and `cache.test.mjs` (marks before
  deletion, a worker replaced mid-mark, eviction scopes, owner filtering,
  storage fallbacks). `parts.js` needs a DOM and is left to the browser
  journeys. `page.mjs` and `esm-hooks.mjs` load the page modules as ES
  modules, since the repository has no `package.json`.

## Changed
- A mark now reaches every worker that may be answering reads: the
  controller and the registration's active worker, looked up again after
  each round until none is new. If a new build takes over as the page
  posts, the old worker may never answer; before, the page waited a second
  and deleted while the new worker, holding no mark, could store an answer
  to a request that started before the deletion. Its successor is now
  marked first. An uncontrolled page (a hard reload) also marks the active
  worker other reader tabs use.
- `just test-worker` needs Node 18.19+ (or 20.6+), for `register` in
  `node:module`, and skips with a message on an older Node, as it does
  without one. The README's Testing section says so.
- A concept route carries its section as a parameter,
  `#/v/<catalog>/<path>.md?s=<slug>`, and its path is every segment, so a
  folder named `x.md` is a folder (review). `fromBrowse` ends a console
  link's document at its last `.md` segment: render.rs links only to `.md`
  paths and a heading slug has no `.` (a new property test,
  `slugify_output_has_no_dot`). The console's own first-`.md` rule is #829.
- `read` turns a 2xx whose body cannot be read (malformed JSON, a dropped
  connection) into a `failed` ReadError instead of answering null data
  (review).
- `READ_CACHES.ownerKey` names the owner record's key once, for the page and
  `sw.js`.
- The in-memory Cache Storage fake moved from `sw.test.mjs` to
  `tests/read_worker/storage.mjs`, shared with the page tests.
- The plan's Task 3 (`names.js`, `OWNER_KEY`) and Task 5 (`cache.js` as
  shipped, the tests) are corrected under dated notes.

## Notes
- A worker that is redundant but still answering a request the browser let
  outlive it cannot be reached by any message. The page still deletes after
  `MARK_WAIT_MS`, so revocation never blocks on a worker.

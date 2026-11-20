# M19 Task 03: the reader's service worker

## Added
- `src/http/read/sw.js`: the reader's service worker, scoped to `/read/`.
  - Install is verified. The shell listing must carry the worker's version,
    and every file must hash, through `crypto.subtle`, to the SHA-256 the
    listing gives. Otherwise the install fails and leaves no shell cache.
  - `activate` deletes older `read-shell-*` caches and claims the open
    pages.
  - Navigations under `/read/` get the cached page. Shell files are served
    cache-first.
  - Concept and document reads go to `read-content`, network-first, capped
    at 200 and least recently viewed out first. Listings go to `read-meta`,
    network-first.
  - Every other API read goes to the network untouched.
- In-memory invalidation marks (`*`, a catalog, a document path), sent by
  the page and answered on its message port. The worker never stores an
  answer to a request that started at or before a matching mark, and checks
  again after writing.
- Account ownership keyed on `X-OKF-Account`. The first answer for a new
  account wipes the old copies and records the new owner in `read-owner`.
  An answer for another account, to a request from before the takeover, is
  never kept. An answer without the header is never cached.
- Offline, a concept request is answered by its other form: a full copy
  answers a plain request, and a plain copy answers a full request when it
  holds the whole body.
- `src/http/read/js/names.js`: the cache names and the cap, shared by the
  page and the worker.
- Tests: `the_worker_is_revalidated_and_carries_the_shell_version` and
  `the_console_does_not_serve_the_reader_worker`. The shell-listing test now
  also checks the served worker's and `names.js`'s hashes.

## Changed
- The plan's worker is corrected (dated note in Task 3), and so is the
  spec's ownership paragraph. When an account took over, the old text set a
  `'*'` mark at that moment. Every request already on its way matched that
  mark, so the new owner's own first answers were never kept, a first visit
  included. Writes also went through a cache handle opened when the request
  started, which the takeover had just deleted.
  - Now an owner change starts a new epoch. A copy is kept only while the
    epoch it was admitted under is current.
  - The cache is opened at write time.
  - Marks and takeover times use the monotonic `performance.now()`.

## Notes
- Cache writes are best effort. When Cache Storage refuses to open, match
  or delete, the worker passes the network's answers through and keeps
  nothing.
- Copies are kept only once `X-OKF-Account` is served (M19.02).
- No browser journey can run the worker yet: the page code arrives in
  Tasks 5 and 6, and the journeys in Tasks 10 and 11. So far it has been
  syntax-checked with `node --check`. It was also run, outside the
  repository, against fake Cache Storage and fetch to check the
  interleavings.

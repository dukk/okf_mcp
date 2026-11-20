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
  account deletes the old copies and the old `read-owner` record, then
  records the new owner. The new account's answers are kept only once that
  record is written; if any step fails, nothing is kept for it, so the
  record never names an account other than the one whose copies are
  cached. An answer for another account, to a request from before the
  takeover, is never kept. An answer without the header is never cached.
- Offline, a copy is served only to the owner. Its own `X-OKF-Account` must
  be the current owner's, decided in turn with takeovers and checked again
  after the read, as is any mark since the request started. With no owner (no record, a wipe, or a takeover that could
  not delete the old copies), nothing is served from `read-content` or
  `read-meta`, and they are deleted (best effort). A copy for another
  account is deleted. Only a checked copy is renewed.
- Offline, a concept request is answered by its other form: a full copy
  answers a plain request, and a plain copy answers a full request when it
  holds the whole body.
- `src/http/read/js/names.js`: the cache names and the cap, shared by the
  page and the worker.
- Tests: `the_worker_is_revalidated_and_carries_the_shell_version` and
  `the_console_does_not_serve_the_reader_worker`. The shell-listing test now
  also checks the served worker's and `names.js`'s hashes.
- `tests/read_worker/sw.test.mjs`: 23 tests that run the worker itself in a
  `node:vm` context, with an in-memory Cache Storage and a fetch the test
  answers. They use Node's built-in test runner and no npm packages. They
  cover parallel first-visit answers, a takeover with a late answer for the
  old account, slow answers after each kind of mark (or during the write),
  a failed owner-record write followed by a restart, offline copies after a
  failed takeover or with the record gone (before and after a restart),
  copies for another account, a copy evicted while it is read, an owner's
  copies after a restart, refused
  storage, the
  short/full fallback, the cap, navigation, and install refusing a wrong
  version or hash.
- `just test-worker` runs them. Without `node`, it prints a skip line and
  passes. `just test` runs it first, so `tools/agentic/gate.sh test` runs it
  too. Documented in the README's Testing section.

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
- Cache writes are best effort, and the page always gets the network's
  answer. When Cache Storage refuses to open, write or delete, the worker
  keeps nothing. When it refuses to read, the worker answers from the
  network.
- Copies are kept only once `X-OKF-Account` is served (M19.02).
- One case stays open. If Cache Storage refuses to delete both the old
  copies and the old owner record during a takeover, the worker keeps
  nothing new and serves nothing until it restarts. A restarted worker then
  finds a consistent record and copies, both the old account's. It serves
  them offline until the next answer, from whichever account is signed in,
  wipes them. That is the same exposure as signing in as someone else and
  going offline before any answer: the worker cannot know about the new
  account until the network names it.
- No browser journey can run the worker yet: the page code arrives in
  Tasks 5 and 6, and the journeys in Tasks 10 and 11. Until then,
  `tests/read_worker` runs it against fakes.

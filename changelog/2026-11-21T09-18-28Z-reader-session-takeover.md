# Reader: an account change seen on an uncached answer takes over the offline copies

Issue #858, found in review of the M19 milestone PR.

## Fixed
- The reader's worker (`src/http/read/sw.js`) decided which account owns the
  offline copies only from answers it caches. Answers it passes through
  uncached (the session check, search) carry `X-OKF-Account` too, but nothing
  read it. So after a second account signed in in another tab, a reader tab
  that only ran boot's session check still treated the first account as the
  owner, and offline served it the first account's copies.
- Every other `GET /api/` read now goes through `passThrough`: the page gets
  the network's answer unchanged and the body is never stored, but a `2xx`'s
  tag goes through `admit`, under the same ordering rules (no takeover by an
  answer to a request that started before the current owner took over, or
  before a wipe). A tagged answer waits in the owner queue, so the page gets
  it only once any takeover it or an earlier answer started has deleted the
  old copies: the page's own "Available offline" list (`offlineDocuments`,
  which reads Cache Storage directly) never lists the old account's copies
  after it, and the worker's offline answers already wait in the same queue.
  This holds for an answer for the new owner too, which may arrive while
  another answer's takeover is still deleting. An answer for the current
  owner, without a tag, or not a `2xx` changes nothing; one without a tag or
  not a `2xx` is not held at all.

## Tests
- `tests/read_worker/sw.test.mjs`: a passed-through answer for a new account
  takes over and wipes before the page has it; one for an owner whose
  takeover is still deleting waits for the wipe; a late one for the old account,
  or one that started before a wipe, changes nothing; one without a tag, for
  the current owner, or not a `2xx` changes nothing.
- `tests/ui_e2e/read.rs`: a new journey caches as the first account, signs in
  as a second in another tab, runs only the session check, goes offline, and
  finds the first account's concept unavailable and Home's offline list
  empty. The two-account journey now checks the copies after the session
  check alone, before reading any concept as the second account.

## Docs
- `docs/reference/reader.md`, the reader design spec's ownership paragraph,
  the M19 changelog and the plan's Task 3 (a dated correction).

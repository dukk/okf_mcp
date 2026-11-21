# watch-review.sh: no more waiting on a PR the bot will skip

## Fixed
- A bot reaction from an earlier review round counted as "the bot saw this
  push", so the ~90 s re-trigger never fired on a later round and the
  watcher waited the long delay instead. Only a reaction made since the
  watcher started counts now.
- A PR that conflicts with `main` (`DIRTY`) was not noticed, and neither was
  the bot's "out of date" answer to a re-trigger: the watcher kept waiting
  until its overall timeout. It now exits 5 at once in both cases, as it
  does for `BEHIND`, which used to wait for the timeout too.

## Changed
- The re-trigger after a bot reaction comes at ~5 min (was ~6 min), and the
  overall timeout is 15 min (was 30 min).
- README rows for `watch-review.sh` and `AGENTIC_WATCH_TIMEOUT`.

## Tests
- `tools/agentic/tests/run.sh`: an earlier round's reaction does not delay
  the re-trigger; `DIRTY` and `BEHIND` exit 5 at once; an "out of date"
  notice after the re-trigger exits 5; the deadline and delay expectations
  follow the new limits; a failed merge-state or comments lookup posts no re-trigger on that pass. 76 passed.

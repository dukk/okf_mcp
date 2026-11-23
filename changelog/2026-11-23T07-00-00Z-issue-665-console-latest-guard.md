# Issue #665: the latest() read guard on four more console lists

## Changed

- `changesets.js`: `load()` took no guard; an older list answer (a Discard
  reload overtaken by another, a Refresh) could draw over a newer one, or
  into a page already left. It now takes a number from `latest(main)` and
  draws only while it is the newest read and the page is shown.
- `my-grants.js`, `my-keys.js`: `load()` checked only `main.isConnected`;
  the check is now `latest(main)`'s `current()`, which adds the ordering. In
  My keys the New key button and the note change only for the newest read.
- `runs.js`: the `loading` flag stays. It serializes reads, and that is
  needed beyond ordering: Load older appends to the rows and cursor drawn,
  so a newer read must not cancel an older one (a counter alone would drop
  the page). It gains `latest(main)` for the other half only: a page that
  answers after the user left commits and draws nothing.

## Not changed

- `unlocks.js`, `keys.js` (the keys list and the codes list) and `audit.js`
  keep their own counters. Each carries extra state with its counter
  (`settled`, `view.latest`, stale markers, label reads), so moving them is
  not a mechanical swap, and none has a held-answer journey yet.

## Tests

- `changesets::an_older_list_read_that_answers_late_never_redraws_the_changesets_table`,
  `self_service::an_older_list_read_that_answers_late_never_redraws_the_my_keys_table`
  and `..._my_grants_table`: the list read after the first Discard or Revoke
  is held (`Ui::hold_answer`) while the second is done; the released older
  answer must not bring the second row back.
- No journey for Runs: a late page into a left page draws into a detached
  view, which the DOM cannot show.
- Revert check: with each page's `current()` check replaced by the old one
  (none for changesets, `main.isConnected` for the other two), all three
  journeys fail with "the older read redrew the ... table".

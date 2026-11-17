# M17 Task 10a: console core, labels in the header and on Account

Issue #607, part of epic #597.

## Added

- `core/labels.js`: `setLabels`, `userLabel`, `teamLabel`, `catalogLabel`
  and `named(label, slug)`. It only looks labels up (the server computes
  them, `GET /api/v1/labels`); an unknown slug shows as itself. A
  `labels` slot in `core/store.js` holds the table.
- Account page: an editable display name (`#account-display-name`,
  `PATCH /me`), an "Account ID" line (`#account-id`), and a "Teams you
  own" section built from `owned_team_details`. A non-admin owner renames
  each shared team there (`PATCH /teams/{name}`); the personal team shows
  "Named after you" with no editor.
- `.named` and `.id-line` styles.
- Browser tests: the header label and the option text versus value
  (`login.rs`), the display-name edit and an 81-character refusal, and
  the owner and plain-member views of "Teams you own" (`account.rs`).

## Changed

- `refreshIdentity` also reads `GET /labels` (quiet: a failed read leaves
  slugs showing instead of failing the identity). The header shows
  `user_label`, and each catalog `<option>` shows the catalog label with
  the slug kept as its value.
- The Account profile lists teams by label, and the ceiling table shows
  catalog labels. The Name row is gone: the display name and the Account
  ID replace it.
- Sign-out clears the label table with the rest of the identity.

## Notes

- Labels are user text and reach the page only through `h()` and
  `textContent`.
- Every new write control is guarded with `core/pending.js`, so a second
  click while a save runs is ignored.
- The display-name input carries `maxlength="80"`, which stops typing but
  not a programmatic value; the server's 422 is the real check, and the
  test exercises it.

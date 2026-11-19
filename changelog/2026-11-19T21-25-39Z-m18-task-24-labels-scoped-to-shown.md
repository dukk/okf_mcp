# M18 Task 24: admin label reads are scoped to the entities shown (issue #749)

For an administrator that reaches every catalog, `GET /api/v1/labels` read
every user and team (`LabelLookup::everyone()`, uncached) on every catalog
page, and the console's identity refresh follows every page: with a million
catalogs, one refresh made about 1,000 whole-population reads and sent the
same maps 1,000 times. Found by the review bot on milestone PR #747.

## Added

- `GET /api/v1/labels/named?users=&teams=`: the labels of the named users
  and teams (comma-separated, at most 100 of each, `{users, teams}`). An
  admin gets every one that exists (it may already list every user and
  team with their labels); anyone else only its own user and its own
  teams. Read through the bounded `LabelLookup::labels_for`.
- Console: `ensureLabels({users, teams})` and `forgetLabels()` in
  `core/labels.js`; `userChoices(rows)` in `components/picker.js`.
- Tests: `labels_tests::labels_named_answers_an_admin_any_and_others_only_their_own`,
  `labels_named_refuses_a_typo_and_too_many_slugs`,
  `an_admin_labels_page_reads_a_bounded_number_of_rows` (50,000 users and
  personal teams seeded by SQL); browser journeys
  `keys::the_keys_table_reads_the_labels_of_the_users_it_shows_only`,
  `keys::a_failed_labels_read_still_draws_the_keys_by_slug`,
  `keys::a_late_labels_answer_never_replaces_a_newer_one` (review of PR
  #754: an answer asked for before a refresh never replaces a label read
  since),
  `unlocks::an_admin_reads_another_staff_members_unlock_by_label`.

## Changed

- `GET /api/v1/labels` answers every caller alike: its own user and teams,
  and the page's catalogs with their owner teams. An every-catalog admin no
  longer gets every user and team (documented in `docs/operations.md`
  Upgrades).
- The keys, sign-in code and admin Unlocks tables read the labels of the
  users their rows name before drawing; a failed read draws the slugs. User
  and team pickers label their options from the `GET /users` and
  `GET /teams` rows they already read; the user drawer's messages use its
  user's `label`.
- `LabelLookup::everyone()`, `Labels::users()`, `Labels::teams()` and the
  picker's `namesOf` are removed.

## Notes

- Privacy: no caller receives a label it could not get before. Named reads
  take the check of `GET /users` and `GET /teams` (any admin), which
  already answer every user's and team's label; anyone else gets only its
  own user and teams, as its `/labels` page did. A catalog-scoped admin's
  keys table and key drawer now show a user's label rather than the slug
  (three keys journeys updated).
- Not changed: `GET /users` and `GET /teams` remain whole admin listings,
  and the shell still follows every `/whoami` and `/labels` page; each page
  is now bounded by the page size.

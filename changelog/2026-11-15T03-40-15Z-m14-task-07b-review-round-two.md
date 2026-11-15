# M14 Task 7b: review round 2, admission at the read and one gates snapshot

PR #433 review (brooke-reviews, round 2). Issue #401.

## Added

- `ToolContext::admit_all(&states, kind)`: all-or-nothing admission for a read that reads every catalog before answering (search). `admission_of` checks with no side effect; `note` queues once per call, catalog and kind.
- Tests:
  - `access_tests::a_read_that_answers_from_the_first_catalog_records_only_that_one` (`history` and `get_concept` on `[main, other]`);
  - `access_tests::a_search_refused_after_one_catalog_passes_queues_no_read`;
  - `store::gates::tests::the_gates_are_one_snapshot_across_a_catalog_replaced_mid_load`.

## Changed

- Admission moved from the selection to the reads:
  - `select`, `states` and `read_set` admit nothing (`resolve_call` is gone).
  - `view` admits the state it views; the `history` loop admits each state before reading its revisions; `one` and `changeset` admit what they return; search uses `admit_all`.
  - `pin` resolves its handle without admitting, through a private `resolve_changeset`.
- `gates::load` reads its maps in one read-only `REPEATABLE READ` transaction, instead of five pooled connections that could straddle a catalog's replacement or transfer. `load_with` is the test seam between its first and later reads.
- `docs/operations.md`: only catalogs actually read count. The plan has dated round-2 notes in Task 7b and 9b.

## Notes

- In a loop that reads catalog by catalog, a `Busy` on a later catalog leaves the earlier ones recorded: they were read.
- Revert checks:
  - admitting every selected catalog in `select` fails the first-catalog test (`other` is recorded);
  - admitting search's catalogs one by one fails the refused-search test (`main` is queued);
  - reading the catalogs on the pool, outside the snapshot, fails the gates test (the replacement's id beside the old membership).

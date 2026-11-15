# M14 Task 7b: review round 3, no admission at target resolution

PR #433 review (brooke-reviews, round 3, final). Issue #401.

## Added

- `access_tests::a_maintenance_mutation_records_no_read_and_a_revision_read_records_one`: `staff_root`, with reporting off, gets `Forbidden` from `maintenance_claim` and queues nothing; its `maintenance_read_revision` records one admin read.

## Changed

- `ToolContext::one` admits nothing. `target`, its non-admitting twin, is folded back into it.
- Tools that read contents admit explicitly, after their own access checks:
  - the REST `tree` route;
  - `maintenance_read_revision` (after the pin is validated);
  - `maintenance_get_run`, `maintenance_list_findings_by` and `maintenance_get_finding`.
- These admit nothing: maintenance claim, renew, submit, close, dispose, the run list and the finding counts.
- The two catalog-incarnation tests call `admit` directly.
- Plan: dated round-3 notes in Task 7b and 9b.

## Notes

- Revert checks:
  - admitting in `one` again fails the new test (a refused claim queues 1 read);
  - dropping the revision read's admit fails it too (0 reads).

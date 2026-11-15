# M14 Task 7b: review round 1, audit-scope races

PR #433 review (brooke-reviews, round 1). Issue #401.

## Added

- Regression tests that force the interleavings:
  - `access_tests::a_read_of_an_old_catalog_map_under_newer_gates_is_refused_not_misrecorded`: the key cache reloaded, the catalog map not.
  - `access_tests::an_old_principal_reading_a_new_catalog_map_is_admitted_as_the_new_catalog`: an old principal over a new map, where its member admission was stale.
  - `store::export::tests::an_export_belongs_to_the_owner_it_read_even_if_the_catalog_moves_or_goes`: the entry's first lock is held between the export's read and its record, while the catalog is transferred, then deleted.
  - `store::export::tests::an_export_whose_entry_fails_fails_and_records_nothing` and `access_tests::an_export_whose_entry_cannot_be_written_answers_500_without_an_archive`: the fail-closed contract.
- `Principal::scope_of(name, catalog_id)`.

## Changed

- `ToolContext::admit(&state, kind)` now returns `Result`. It admits only through gates that know the incarnation the call reads (the state's `catalogs.id`): the principal's own, else the key cache's current ones. When neither does, an admin credential is refused with `Busy` (retryable), so a read is never recorded under the wrong catalog or team, nor lost as a stale member's. `select`, `one` and `changeset` propagate the error.
- `Principal::access_record` takes the verified `CatalogScope`.
- The export reads the owner team's audit id in its read-only snapshot. `record_export` writes the entry with that fixed scope, so a transfer or deletion before the entry no longer moves it or drops its team. Its details gain `catalog_id`, and its transaction locks only the actor's people.
- `docs/operations.md`: the export entry's `catalog_id` and scope, and the `Busy` refusal during a reload.
- Plan: dated notes in Task 7b and 9b.

## Notes

- Revert checks:
  - matching gates by name alone fails both incarnation tests (the read is recorded with the wrong scope, and the stale member admission records nothing);
  - resolving the export's teams in the later transaction fails the transfer and deletion test (`["family"]`, then `[]`);
  - ignoring the record's error fails both fail-closed tests.

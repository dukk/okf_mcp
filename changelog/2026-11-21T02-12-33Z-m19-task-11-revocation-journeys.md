# M19 Task 11: revocation journeys (2026-11-21T02:12:33Z)

Issue #818, Task 11 of `docs/superpowers/plans/2026-11-20-mobile-reader-pwa.md`; also
issue #837 (booting with copies already on the device). These are tests only. No reader code
changed, because no journey found a bug.

## Added

- Journeys in `tests/ui_e2e/read.rs`:
  - a revoked session: `a_revoked_session_wipes_the_caches_on_the_next_answer`,
    `a_401_signs_out_without_waiting_for_a_sibling_read` and
    `a_late_401_after_an_ordinary_refusal_still_signs_out`;
  - a removed grant: `a_removed_grant_evicts_that_catalog_only`,
    `a_lost_grant_answered_after_navigating_away_still_evicts` and
    `a_late_refusal_from_home_evicts_the_catalog_it_asked_about`;
  - an unlock refusal: `an_unlock_refusal_keeps_the_caches_and_points_at_the_console`;
  - slow successes after an eviction: `a_slow_success_cannot_put_back_an_evicted_catalog`,
    `a_slow_success_cannot_put_back_a_deleted_concept` and
    `a_slow_success_cannot_put_back_a_deleted_folder_index`;
  - a deleted folder index: `a_folder_whose_index_is_deleted_meanwhile_still_lists_its_concepts`;
  - a long concept: `an_outline_link_in_a_long_concept_keeps_the_whole_body`;
  - two accounts: `signing_in_as_someone_else_in_another_tab_never_mixes_their_copies`;
  - another build: `a_shell_from_another_build_is_not_installed`;
  - booting with copies on the device (#837): `a_401_at_boot_deletes_the_copies_already_on_the_device`
    and `offline_at_boot_keeps_the_copies_and_says_the_account_needs_a_connection`.
- Helpers in `tests/ui_e2e/harness.rs`:
  - `Ui::fail_next_kind`: the `FAIL_NEXT` body now takes `arg.kind`;
  - `Ui::poll_eval` and `Ui::hold_answer_later`;
  - `Ui::pause_worker_responses` and `PausedResponses`: DevTools `Fetch` at the `Response`
    stage on the service-worker target;
  - `Ui::in_second_tab`: a second tab in the same browser context;
  - `Ui::tamper_new_workers` and `Tamper`: auto-attach to each new service worker, paused
    until `Fetch` is on, then rewrite matching responses. `Tamper` counts what it rewrote.
  - `DevTools::send`.
- Helpers in `tests/ui_e2e/read.rs`: `publish`, `main_files_without`, `revoke_sessions`,
  `lose_main`, `content_paths`, `wait_held` and `wait_answered` (Resource Timing).

## Changed

- `DevTools::call` keeps the events that arrive while it waits for an answer, and
  `DevTools::event` looks at those first. Before, they were dropped.
- `home_and_search_without_a_connection` used to remember `main` by opening its tree. The
  tree reads the root `index.md`. When the worker kept that copy, home listed it as available
  offline, so "Nothing yet" never appeared. Which happened depended on timing: the journey
  failed in `test-ui` and in 3 of 3 small parallel runs. It now opens `#/s/main`, which
  remembers `main` without reading anything, and it waits for "Nothing yet" instead of reading
  `#reader` once.

## Decisions

- Every new journey was checked by breaking the behaviour it guards, then restoring it. Each
  one failed under its break. The breaks were: no takeover in `admit`, no hash check at install,
  no wipe in `signIn`, no wipe on boot's 401, a wipe on an offline boot, no eviction for a lost
  grant, eviction of the current catalog instead of the asked one, effects only while the
  screen is live, eviction on `LOCKED`, no unlock link, no `ctx.settle` on the concept's reads
  or on its links read, the worker ignoring catalog or document marks, a deleted index treated
  as a failure or left cached, and a section route without `?full=true`.
- Two journeys passed under their breaks on the first try, and were tightened. The outline
  journey read the old body before the section route redrew, so it now marks `#body` and waits
  for it to be drawn again. The late-401 journey could revoke the session before the 404
  arrived, so it now waits for the 404's Resource Timing entry first. The lost-grant journey
  waits the same way for its links read.
- The DevTools assumptions in the plan hold. The plan has a dated correction for the places
  where the shipped code differs: the `base64` crate replaces a hand-written encoder, the
  other-build poll is now `!r || ...` because a failed first install clears the registration,
  and held concept screens are awaited through `window.heldMatches`.

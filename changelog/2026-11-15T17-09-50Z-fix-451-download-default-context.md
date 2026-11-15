# Fix #451: the console download helper falls back to the default context

## Fixed

- `tests/ui_e2e/harness.rs`'s `Ui::download` opens its own DevTools
  connection and called `Browser.setDownloadBehavior` with the page's
  `browserContextId`. rustwright creates that context on its own
  connection, and Chrome (seen with Chrome for Testing 147) answers any
  other connection with `Failed to find browser context`, so both audit
  export journeys failed on every run:
  `audit::an_admin_filters_the_log_by_action_and_exports_it` and
  `audit::an_owner_reads_its_team_and_sees_an_operator`.
- On that error the helper now repeats the call without
  `browserContextId`, applying it to the default context, as rustwright's
  own `enable_downloads` does. Any other error still fails the test.
- `DevTools::try_call` returns Chromium's error answer instead of
  panicking; `call` wraps it.

## Tests

- All six `audit::` journeys pass, the two export journeys included.

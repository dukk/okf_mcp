# Test: remove the load-sensitive waits behind the known flakes (issue #500)

Each test on the issue was read against the code first. Some were already fixed, and the rest had a wait that assumed an idle machine.

## Already fixed (no change here)
- `okf::caller` tests, `http::tests::an_internal_request_error_exports_its_kind_and_not_its_text`, `oauth::authorize::tests::a_refused_client_id_reaches_stderr_and_not_otlp` and `oauth::token::tests::oauth_tokens_count_by_grant`: the first-registration race in `tracing-core` (issue #481). The `Observer` and `SpanRecorder` that the last three use install their subscriber through `crate::test_tracing`, and `Observer::finish` drops the dispatcher, then flushes both providers, before it reads the exporters, so nothing is read before a span ends.
- `app::tests::a_loaded_catalog_that_keeps_failing_to_refresh_counts_failed_every_pass`: the test no longer exists. M18 task 8 replaced it with the catalog cache's tests (`src/cache/catalogs_tests.rs`), whose waits are bounded by 30 s.
- `investigator_claude_stdin_timeout_cancel_reap`: issue #307 (PR #636) made it retry a run whose child had not started when the short deadline fired.

## Changed
- `tests/ui_e2e/harness.rs`: `WAIT`, the longest a `wait_*` polls, goes from 10 s to 60 s. A poll returns the moment its condition holds, so only a late page waits that long.
- `tests/ui_e2e/account.rs`: a new `open_account` opens the Account page and waits for `#main .view[data-loaded=true]`. Every journey that clicked on the page straight after opening it uses it (the page marks itself loaded once all six of its reads settle, a failed read included). The journeys that must see the page before it is loaded (a held or failed read) and the key journey (a key's session is refused the page, which never loads) still use `ui.open`.
- `src/db.rs`: `a_timed_out_start_labelled_leaves_no_container` accepts any timeout-class error (a zero budget can run out in the pull or create stage on a loaded Docker). `a_timed_out_start_leaks_without_cleanup_and_remove_labelled_reaps_it` repeats the start, under a fresh label each time, until one attempt leaves a container behind. `is_timeout_class` moved from `tests/tls.rs` to `tests/support/pg.rs` so both share it.
- `src/investigator/claude_tests.rs`: `Fake::started` and the wait for a killed child to leave `/proc` poll for up to 60 s instead of 5 s. `investigator_claude_timeout_includes_pipe_eof` no longer measures elapsed time against 2.9 s. Its pipe-holding descendant now stays until the test releases it (`tests/fixtures/investigator/claude/fake_child.py`), so returning at the deadline with the descendant alive is the proof, and a run whose child had not started when the deadline fired is repeated with a longer one, as `run_timed_after_start` does.
- `src/investigator/client_tests.rs`: in `investigator_client_paginated_setup_can_be_dropped_at_deadline_or_cancel` the 300 ms deadline counts from the moment the second page list is in flight, not from the start, so a slow machine can neither fire it before the cancel nor before the list. The held second page no longer answers after a fixed 4 s sleep: it waits on a semaphore the test releases only after the setup future is dropped (with a 60 s bound that only ends a stuck test), so it cannot win against the deadline or the cancel (review of PR #920). The elapsed-time assertion is gone.
- `tools/agentic/lessons.md`: two rules, for a test deadline and for a start that can fail in any stage.

## The sign-in page's empty response
The `ERR_EMPTY_RESPONSE` and connection resets on `/signin` were reproduced (a stress run on 4 cores with 16 busy loops) and traced. The journeys open `http://localhost:<port>`, and Chromium tries `::1` before `127.0.0.1`. The test server binds `127.0.0.1` on a port the OS picks looking only at IPv4, and the same number can be a listener of another process on `[::]` only (a container engine's port proxy publishes each port on both families, and some outlive their container). That listener, not the server, then answers the browser. It does not depend on load; it depends on how many such listeners exist, and a busy machine running many test containers has more. `curl http://localhost:<port>/` against a `127.0.0.1` server on such a port gives "Empty reply from server" while `127.0.0.1` works.

Evidence, from the stress run that failed (run 3, 4 cores, 16 busy loops, before the rule existed):
- The journey's error was `open the sign-in page: Message("Page.goto: net::ERR_EMPTY_RESPONSE at http://localhost:43227/signin")`.
- `ss -ltn` on the machine showed `[::]:43227` listening and nothing on `0.0.0.0:43227`, so the kernel could hand `127.0.0.1:43227` to the test server.
- Binding `127.0.0.1:43227` in a script and requesting it: `curl http://127.0.0.1:43227/` returned the page, and `curl http://localhost:43227/` failed with "Empty reply from server".
- Not run: a direct trace of Chromium's address order. That it tries `::1` first is the reading consistent with the above and with the regression test below, which fails without the rule.

- `tests/ui_e2e/harness.rs`: Chromium launches with `--host-resolver-rules=MAP localhost 127.0.0.1`. The origin stays `localhost`, so the secure context and WebAuthn's relying party are unchanged.
- `tests/ui_e2e/smoke.rs`: `localhost_reaches_the_ipv4_server_when_another_process_holds_the_ipv6_port` binds a squatter on `[::1]:<port>` that closes every connection, serves the page on `127.0.0.1:<port>`, and loads it through `localhost`. It fails without the rule (checked by taking the rule out) and passes with it. It skips without an IPv6 loopback.

## Notes
- Issue #440 (the passkey-row timeout) is the same family as the others on the Account page: the journey now waits for the whole page before it waits for the row, and for 60 s instead of 10 s.

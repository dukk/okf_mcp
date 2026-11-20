## Added
- Nothing.

## Changed
- `ntp::tests::a_failed_address_falls_through_to_the_next` no longer drops a bound UDP socket to get a "closed" port. Its failing address is a live fake server answering stratum 16 (a `BadReply`), held until it has answered the request, and the call is bounded with `tokio::time::timeout`.

## Notes
- Issue #707. A parallel test could re-bind the dropped port and swallow the request, so the query hung. TCP bind-then-drop tests elsewhere (`src/investigator/client_tests.rs`, `src/mail/postmark.rs`, `src/mail/ses.rs`) share the pattern but are not NTP tests and are left alone.

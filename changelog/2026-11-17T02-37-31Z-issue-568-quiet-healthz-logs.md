# Issue 568: quiet `/healthz` console request lines

`record_response` (src/http/mod.rs) logged one INFO completion line per
response, so every health probe wrote a line to stderr. `request_telemetry`
knows the matched route, so it now marks a `/healthz` response with a
`RoutineProbe` extension, and `record_response` logs that response's
completion line at DEBUG instead of INFO.

- Applies to text and JSON console formats (the level is the event's own).
- The request span, its status and error attributes, and the HTTP metrics
  are unchanged. Other routes, including catalog health endpoints, still log
  at INFO. The query string is not part of the matched route, so
  `/healthz?x=1` is covered.
- OTLP log export is unchanged: the completion line's target is
  `tower_http::trace::on_response`, which the OTLP allow-list
  (`otlp_cap`, `src/telemetry/subscriber.rs`) already excluded at every
  level (see `dependency_events_never_reach_otlp`).
- Tests: `health_probes_write_no_completion_line_to_the_console`,
  `other_routes_keep_their_completion_line`,
  `a_failing_health_probe_is_quiet_but_still_exported_as_an_error`.

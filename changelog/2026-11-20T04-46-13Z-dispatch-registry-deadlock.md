## Added
- `telemetry::subscriber` tests `dropping_a_subscriber_never_drops_its_providers` and `captures_made_and_dropped_on_many_threads_do_not_deadlock` (the second aborts the test binary after 120 s instead of letting it hang).

## Changed
- `telemetry::subscriber::build_with_writer` keeps a clone of each OpenTelemetry tracer and logger provider it builds a subscriber on until the process exits, so dropping a subscriber never drops a provider. `capture::Observer::finish` empties its in-memory exporters after reading them, since their providers now live on.

## Notes
- Issue #738. A provider's `Drop` emits an opentelemetry internal-logs `tracing` event even after shutdown. When the last handle on a provider was the subscriber, and the subscriber's last handle was the momentary one `tracing_core` takes while rebuilding interest under its dispatcher registry lock, that event's first callsite registration took the lock again and every thread that registers a callsite or a subscriber deadlocked. Production builds one such subscriber, the global default, which is never dropped, so keeping its providers changes nothing there.
- The other tests that install a scoped subscriber (`rows_returned::counting`, `cli`, `http`, `http::api::identity`, `okf::caller`, `telemetry::json`, `SpanRecorder`) build theirs from `tracing_subscriber` layers alone, or through `build_with_writer`; dropping one emits no event, so none of them can be the subscriber dropped inside the lock. They still take the registry's write lock, which only made the cycle above more likely to close.

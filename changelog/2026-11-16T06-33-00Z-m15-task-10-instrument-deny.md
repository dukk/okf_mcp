# M15 Task 10: uninstrumented_fn at deny

Issue #475.

- `just dylint` now passes `-D uninstrumented_fn` (was `-W`). The crate roots were touched so cargo re-checked at the new level.
- `src/lib.rs` gains `#![recursion_limit = "256"]`: the cold nightly check overflowed the default query depth on the future of `app::load`.
- Deny proved: a probe `pub fn` without `#[instrument]` made `gate.sh dylint` exit 1; with it removed the gate exits 0.
- Re-ran the request-span and tool-span root tests, the clock-check root test and the propagation tests at both `info` and `trace` filters (45 tests passed), and `telemetry_privacy` (5 passed, trace for traces and logs).
- `docs/contributing/policy-lints.md` and `tools/agentic/brief-common.md` now say the lint is deny and `gate.sh dylint` is required.

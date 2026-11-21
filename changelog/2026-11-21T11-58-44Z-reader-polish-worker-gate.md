# Agentic tools: a gate for the reader's Node tests

Issue #862, Task 0 of the reader-polish plan.

`tools/agentic/gate.sh worker` runs `just test-worker` (the reader's Node tests
in `tests/read_worker/`) under a hard timeout, so agents no longer run those
tests outside `gate.sh`. It takes no lock (the tests are light and `just test`
already runs them) and has a 5 minute limit, overridable with
`AGENTIC_TIMEOUT_WORKER`. A timeout exits 124 like every other gate.

- `tools/agentic/gate.sh`: the gate, its header line and usage text.
- `tools/agentic/tests/run.sh`: two shell tests (the recipe it runs, and the 124 on timeout).
- `tools/agentic/README.md`, `tools/agentic/brief-common.md`: documented.

# Agentic tools: a gate for the reader's Node tests

Issue #862, Task 0 of the reader-polish plan.

## Added
- `tools/agentic/gate.sh worker` runs `just test-worker` (the reader's Node tests in `tests/read_worker/`) under a 5 minute hard limit, overridable with `AGENTIC_TIMEOUT_WORKER`. It takes no lock: the tests are light, and `just test` already runs them. A timeout exits 124 like every other gate.

## Tests
- `test_gate_worker_runs_the_just_recipe` and `test_gate_worker_timeout_returns_124` in `tools/agentic/tests/run.sh`.

## Docs
- `tools/agentic/README.md` (the gate row and the limits row) and `tools/agentic/brief-common.md` (the gates block).

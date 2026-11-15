# M15 Task 9b: instrument okf, overlay, render, selection

## Added
- `#[tracing::instrument(level = "trace", skip_all)]` on private helpers and
  `level = "debug"` on public entry points (and `Workspace` trait impls) in
  `src/okf/**`, `src/overlay.rs`, `src/render.rs` and `src/selection.rs`.
  320 functions are instrumented (151 debug, 169 trace). No span records a
  field, so nothing from a path, document text or error can reach a span.
- 87 `uninstrumented_fn` exemptions, each with an accepted reason:
  - 32 per-element predicates or scanners in the parser and analysis hot loops;
  - 28 trivial accessors;
  - 20 one-line delegations;
  - 5 trivial constructors;
  - 2 span makers (`Caller::capture` and `Caller::enter`).
- `src/okf/caller.rs`: `Caller`, a helper that carries the calling thread's
  tracing span and dispatcher onto rayon workers. Rayon workers do not inherit
  tracing's thread-local span or a scoped subscriber, so without it a span
  opened inside a parallel pass is a root span, detached from the operation (and
  from the request or publish trace) that started it. `Caller::capture()` runs
  before the parallel iterator and `map_init(|| caller.enter(), ...)` re-enters
  the context once per rayon work split on each worker.
- Three tests in `okf::caller` (a 4-thread pool and a recording layer): workers
  without the helper see no scoped subscriber; with it, 64 worker spans keep
  `root` as parent and none run on the test thread; and `Snapshot::build` over
  3000 documents records every `read_document` span as a child of `build`. The
  last test fails when `map_init` is removed from `build`.

## Changed
- Seven parallel passes now use `map_init` with the helper: `Snapshot::build`
  and its secret scan, lint, oldest, sprawl, search and unlinked mentions. This
  changes tracing behaviour only (worker spans now have the right parent and
  subscriber); the values these functions return are unchanged, and the
  existing tests for them pass untouched.
- The dylint warning count in these paths goes from 405 to 0. Other paths keep
  their warnings until their sweep PRs merge.
- The plan's Tasks 9a-9e procedure has two dated "Corrected (issue #471)"
  notes: exemptions are applied per function rather than by moving items into
  an exempt `impl`, and parallel passes must propagate the caller's span with
  the helper.

## Notes
- Apart from `src/okf/caller.rs` and the seven `map_init` call sites, only
  attributes were added to the swept paths.
- The hot-loop exemptions are the per-char, per-line and per-document
  helpers in `pystr`, `paths`, `yaml`, `markdown`, `timestamp`, `search`,
  `ops/mod`, `ops/replay`, `analysis/oldest`, `analysis/lint`, `snapshot`
  and `doc`; the PR lists each by name.
- The privacy sentinel test is not yet available on main.

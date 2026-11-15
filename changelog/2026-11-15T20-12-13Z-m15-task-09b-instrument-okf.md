# M15 Task 9b: instrument okf, overlay, render, selection

## Added
- `#[tracing::instrument(level = "trace", skip_all)]` on private helpers and
  `level = "debug"` on public entry points (and `Workspace` trait impls) in
  `src/okf/**`, `src/overlay.rs`, `src/render.rs` and `src/selection.rs`.
  320 functions are instrumented (151 debug, 169 trace). No span records a
  field, so nothing from a path, document text or error can reach a span.
- 85 `uninstrumented_fn` exemptions, each with an accepted reason:
  32 per-element predicates or scanners in the parser and analysis hot loops,
  28 trivial accessors, 20 one-line delegations, 5 trivial constructors.

## Changed
- The dylint warning count in these paths goes from 405 to 0. Other paths keep
  their warnings until their sweep PRs merge.
- The plan's Tasks 9a-9e procedure notes that exemptions are applied per
  function rather than by moving items into an exempt `impl`.

## Notes
- No behaviour change: only attributes were added.
- The hot-loop exemptions are the per-char, per-line and per-document
  helpers in `pystr`, `paths`, `yaml`, `markdown`, `timestamp`, `search`,
  `ops/mod`, `ops/replay`, `analysis/oldest`, `analysis/lint`, `snapshot`
  and `doc`; the PR lists each by name.
- The privacy sentinel test is not yet available on main.

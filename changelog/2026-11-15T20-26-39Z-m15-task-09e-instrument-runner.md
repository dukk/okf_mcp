# M15 Task 9e: instrument the runner (investigator, bin, maintenance)

## Added
- `#[tracing::instrument(skip_all)]` on every function in `src/investigator/**`,
  `src/bin/okf-maintenance.rs` and `src/maintenance/**` that the `uninstrumented_fn`
  lint flagged: `level = "debug"` for `pub`/`pub(crate)` entry points, `level = "trace"`
  for private helpers.
- Safe span fields on `Controller::submit` and `Controller::record_model`: the work
  index, as `i64`.
- Exemptions with accepted reasons: trivial accessors and conversions, `Display`/`Debug`
  impls, per-element predicates, one-line adapters, pre-subscriber code, and constant
  trait plumbing.

## Changed
- `maintenance::types::Submission` moved into a private inline module that is re-exported
  under the same path, so the `schema_with` derive wrapper can carry an exemption.
- Plan Task 9e carries a dated correction note about that pattern.

## Notes
- No behaviour change. `okf-maintenance` keeps its fixed `quiet_logging` filter, so these
  spans are recorded nowhere today. They are instrumented so a future OTLP export needs no
  second privacy pass: no argument, evidence, prompt, token, path or error text is
  recorded, and none of the spans uses `err` or `ret`.
- `uninstrumented_fn` warnings in these paths: 176 before, 0 after.
- The privacy sentinel test does not exist on `main` yet.

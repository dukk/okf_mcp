# M15 Task 9a: instrument the store

Issue #470, part of epic #460.

## Added
- `#[tracing::instrument(skip_all)]` on every function in `src/store/**` that the
  `uninstrumented_fn` lint flagged (321 functions): `level = "debug"` for
  `pub`/`pub(crate)`/`pub(super)` entry points, `level = "trace"` for private helpers.
- A safe span field where the function takes one: `catalog_id` or `changeset_id`, both recorded with their value,
  `i64` ids.
- 96 per-function exemptions with a reason: enum `as_str`/`parse`, accessors and
  conversions, error-value builders, `Debug` impls, and a few small pure
  conversions (arithmetic or formatting, no I/O).

## Notes
- Spans only: no SQL, transaction, lock order or control flow changed.
- No argument, SQL, bind value, path, document text, user name, email, key secret or token
  is recorded, and no span uses `err` or `ret`.
- Helpers under `okf.publish` are `trace` spans that nest in the existing `okf.publish*`
  spans; none wraps them again. The function that already carried its own span is unchanged.
- `uninstrumented_fn` warnings in `src/store`: 417 before, 0 after.
- The privacy sentinel test does not exist on `main` yet.

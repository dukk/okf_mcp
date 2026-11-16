# Live parity tests normalize the intentional wording changes (issue #547)

Issues #515 and #532 changed three runtime messages on purpose (the sprawl lint warning, the
two `split_concept` notes) and the `index` missing-title refusal, and hand-edited the goldens.
The ignored live tests still compared okf.py's text exactly, so they failed.

`tests/parity.rs` now has one list, `INTENTIONAL_TRANSLATIONS`. It does three jobs:

- `translate_python` rewrites okf.py's output into the server's wording before
  `live_analysis_parity`, `live_ops_parity` and `regenerate_goldens` compare or write it.
- `goldens_carry_the_telekin_wording` checks the hand-edited golden lines against it.
- Unit tests cover each mapping, the exact old strings, nested values, and keys left alone.

Everything else is still compared exactly. The live tests were not run: they need okf.py,
which this work must not execute.

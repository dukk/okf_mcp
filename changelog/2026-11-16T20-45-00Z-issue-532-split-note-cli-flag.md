# Issue 532: runtime notes name tool parameters, not CLI flags

`split_concept` told clients "no --description given" and "named with --source". A client sends
tool parameters, so the notes now say `no description given` and `named with sources`. The
`index` tool's refusal said "pass --title"; it now says "pass title".

## Sweep of client-facing text

Changed:

- `src/okf/ops/split.rs`: the no-description note and the "named with --source" source note.
- `src/okf/ops/log_index.rs`: the missing-title refusal of `index`.

Left, because only the command line shows them (or the HTTP layer already rewrites them):

- `src/store/catalogs.rs` (`pass --force`) and `src/store/import.rs` (`pass --replace`): the
  catalog admin command and the HTTP layer, which rewrites them to `?force=true` and
  `?replace=true`.
- `src/store/keys.rs` (`pass --actor`): the key-management command line.

## Tests

- `tests/parity.rs` `ops_output_never_names_a_cli_flag`: no string the ops script produces
  contains a `--flag`.
- The split and index unit tests and `tests/golden/ops.json` carry the new wording. The golden was
  edited by hand; a note in `tests/parity.rs` says to reapply it after regenerating from okf.py.

# Issue 538: HTTP refusals no longer name a CLI flag

Two refusals an HTTP API client receives named a command-line flag it cannot use.

- `src/store/keys.rs`: granting or removing a catalog on an every-catalog key said
  `is granted every catalog (--all)`. It now says `is granted every catalog`. The store code is
  shared with the CLI, but the sentence is true for both callers without the flag, and no test or
  CLI path depended on it, so the shared wording changed instead of adding an HTTP rewrite.
- `src/http/api/mod.rs` `require_every_catalog`: the 403 said `(okf-mcp key create --all)`. It now
  says `this needs an admin key that reaches every catalog`, the same words
  `src/http/api/catalogs.rs` already used.

## Tests

In `src/http/api/keys.rs`: both messages as an HTTP client receives them, each also asserting the
text holds no `--`. No broad guard over all HTTP reasons was added: reasons are built at many
sites, and the CLI-visible `pass --force` and `pass --replace` hints are rewritten at the HTTP
layer, so a deterministic and narrow check is not available.

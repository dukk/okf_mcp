## Added
- The signed-in OAuth consent page puts its catalog checkboxes, level selects and default-catalog select inside a native `<details>` disclosure labelled **Advanced Options** (issue #718). It needs no script, so the page's Content-Security-Policy is unchanged.
- With exactly one catalog offered, the disclosure starts closed and that catalog starts ticked, at `write` when the ceiling allows write (else `read`, the only option), and selected as the default catalog. A line above the form says what Approve grants as the page starts.
- Unit tests in `src/oauth/consent.rs` and flow tests in `src/oauth/authorize.rs` for zero, one read-only, one write-capable and several offered catalogs, approval of the untouched one-catalog form, and changed choices. Two browser journeys in `tests/ui_e2e/consent.rs`: Approve on the closed one-catalog form, and opening Advanced Options to change it.

## Changed
- `docs/operations.md` describes Advanced Options and the one-catalog defaults.

## Notes
- The defaults are the form's starting state only. `consent_as_user` checks the posted choices exactly as before: an empty choice, a default outside the choice and anything outside the offered ceiling are refused, and nothing is granted that the person did not submit.
- With several catalogs the defaults are unchanged (nothing ticked, `read`, default None) and the disclosure starts open.
- When a refused choice shows the page again, the disclosure starts open so the controls the message is about are in view.
- The empty-ceiling page and the sign-in-code flow are unchanged.

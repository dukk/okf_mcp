# M17 Task 8: emails, sign-in, consent and passkeys by label (issue #605)

## Added
- Tests for each surface: the queued setup and unlock emails, the setup page, the signed-in consent page, the sign-in-code summary and the passkey registration options, each with hostile labels (markup, quotes, a line break where a header is involved).
- `src/mail/templates.rs`: the unlock email's subject turns control characters and U+2028/U+2029 in the catalog into spaces, so a label can never start a second header line.

## Changed
- The setup email's `user` is the user's label (display name, else the email's local part, else "Unnamed user"), never the slug. The template field names are unchanged.
- The unlock email's `catalog` is the catalog's label, read in the unlock's transaction.
- The setup page's welcome shows the user's label, HTML-escaped.
- The signed-in consent page shows the user's label and each catalog's label as visible text (escaped for text and attributes); slugs stay in ids, names and values. Catalogs sharing a label are told apart by `Labels::catalog_choices`.
- The sign-in-code summary shows the user, catalogs and default catalog by label.
- The passkey registration options' `user.displayName` is the user's label; `user.name` stays the email (or the slug when there is none).
- `UserPage.user` became `user_label`, with a new `catalog_labels` field.

## Notes
- A label is shown from the key cache's directory, so the consent page and summary lag a label change by at most one poll, as every other cached view does.
- A slug unknown to the directory (created since the last load) is shown as itself in the summary.
- The plan's Task 8 has a dated correction note for the shape changes above.

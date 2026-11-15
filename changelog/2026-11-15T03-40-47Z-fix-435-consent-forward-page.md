# Fix: Approve reaches a client whose redirect URI redirects again

Issue #435.

## Fixed
- Approve (and Deny) on the consent page did nothing when the client's
  redirect URI itself redirects to another origin. Cursor's does:
  `https://www.cursor.com/...` answers with a 308 to `https://cursor.com/...`.
  The answer was a 303, and browsers check the page's CSP `form-action` on
  every redirect after a form POST, the client's own included, so the 308
  was blocked. The server logged the consent and a 303, and the client never
  asked for a token. A second click hit the spent pending authorization and
  showed the expired-link page.

## Changed
- `src/oauth/consent.rs`: `forward_page(target, brand)` answers 200 with a
  zero-delay `<meta http-equiv="refresh">` and a "continue" link
  (`#consent-forward`) to the client. A refresh is not a form submission, so
  `form-action` does not apply to it or to any redirect after it. Still no
  script, `no-store`, `no-referrer`.
- `src/oauth/authorize.rs`: `back_to_client` and `error_to_client` take a
  `Hop`. Approve (`finish`) and Deny use `Hop::Forward`. The GET's errors
  after the redirect URI is trusted keep `Hop::Redirect`, a 303, since no
  form is involved.
- The consent pages' CSP is now the constant
  `consent::CONTENT_SECURITY_POLICY`, with `form-action 'self'`. No form's
  answer leaves the server any more, so the redirect URI's origin is no
  longer listed, and `PageView::redirect_origin` is gone.
- `docs/operations.md`: the endpoint table describes the forwarding page,
  and troubleshooting has a row for a browser that doesn't follow it.

## Tests
- Unit and integration tests read the client URL from the forwarding page
  (`oauth::testing::forwarded`, and `forwarded` in `tests/support/oauth.rs`).
- `consent::tests::the_forward_page_refreshes_and_links_to_the_escaped_target`.
- Browser journey
  `approve_reaches_a_client_whose_redirect_uri_redirects_to_another_origin`:
  the redirect URI is a second local port that 308s to the client page on a
  third.

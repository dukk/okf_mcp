# First setup reaches an onboarding guide on another origin (issue #838)

## Fixed

- A first password setup with `onboarding_url` set no longer stops on the setup page. Since
  issue #793, `POST /account/setup` answered with a 303 to the guide. Browsers apply
  `form-action` to every redirect that follows a form POST, and the setup page runs under
  the console CSP (`form-action 'self'`), so a guide on another origin
  (`https://docs.telekin.ai/...` from `app.telekin.ai`) was blocked and the browser stayed
  put. The password was already set and the session cookie already sent, so the setup link
  was used up. The setup now answers with a 200 page ("Opening the getting-started guide")
  that refreshes to the guide and links to it (`#setup-forward`), still setting the session
  cookie. A refresh is not a form submission, so `form-action` does not apply to it. This is
  the same approach the consent page's Approve and Deny use (issue #435).
- A setup that goes to a same-origin path (the console, or an OAuth authorization it was
  continuing) still answers with a 303.

## Changed

- `http::pages::forward_body(heading, link_id, target)` builds the forwarding page's body
  (the meta refresh, a heading and the continue link, everything escaped). The consent
  page's `forward_page_html` and the setup's `guide_forward_page` both use it.
- `after_setup` returns `AfterSetup::Guide(url)` or `AfterSetup::Return(path)`, so the
  handler picks a forwarding page or a 303 by the variant, not by looking at the string.

## Tests

- `ui_e2e` `signin::a_first_setup_lands_on_an_onboarding_guide_on_another_origin`: in
  Chromium, a setup link with `onboarding_url` on another port ends on that page, with no
  console errors (so no CSP violation) and with a session. With the old 303 in place, it
  fails ("timed out waiting for #guide").
- `pages::tests::a_forward_body_refreshes_and_links_to_the_escaped_target`.
- The `signin` setup tests now check for the forwarding page's target instead of a
  `Location`.

## Not changed

- The `static.cloudflareinsights.com/beacon.min.js` CSP error in the same console comes
  from Cloudflare Web Analytics, which the CDN injects into the page. It has nothing to do
  with the redirect: blocking the beacon only loses analytics. To make it go away, turn off
  Web Analytics' automatic injection for `app.telekin.ai` in Cloudflare. Allowing the
  beacon in the console CSP would be the other option, but it isn't recommended.

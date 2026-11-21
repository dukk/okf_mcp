# A welcome page between a first setup and the onboarding guide (issue #846)

## Changed

- A first password setup with `onboarding_url` set now answers with a welcome page, not a
  page that refreshes to the guide at 0 seconds (issue #838). Nobody saw the old page, and
  landing on the docs site felt like being sent out of the app. The new page says "Your
  account is ready", "Thanks for joining {product}. You're signed in.", and has a primary
  button-styled link to the guide (`#setup-guide`) and a link to the console
  (`#setup-console`); the heading is `#setup-welcome`. `{product}` is the branding
  setting's product name, escaped.
- There is no timer and no `<meta http-equiv="refresh">`: a timed refresh the person cannot
  control fails WCAG 2.2.1 (technique F41), and any delay is too short for slow readers or
  a wait for fast ones. The page is a 200 with the session `Set-Cookie`, no `Location` and
  no form. A link click is not a form submission, so the console CSP's `form-action 'self'`
  does not block it.
- The button is an `<a role="button">`, which Pico CSS already styles as a button, so there
  is no new CSS and no inline style (the CSP forbids it).
- `signin::guide_forward_page` is replaced by `welcome_page`. `pages::forward_body` stays:
  the consent page uses it.

## Unchanged

- Without `onboarding_url`, a setup answers with a 303 to its `return_to`. A setup that
  continues an OAuth authorization still gets a 303 to it. Sign-in, reset, failed setups
  and passkey setup behave as before.

## Tests

- The `signin` setup tests now pin the kind of answer: a 303 (`redirect_target`) or the
  welcome page (`welcome_guide`: 200, no `Location`, no refresh, no form, both links). A
  new test escapes a hostile product name and guide URL.
- `ui_e2e` `signin`: the cross-origin journey sets the password, sees `#setup-welcome`,
  checks the browser is still on this origin, clicks `#setup-guide` and lands on the
  guide. A second journey clicks `#setup-console` and sees the console signed in. Both
  assert no console errors.

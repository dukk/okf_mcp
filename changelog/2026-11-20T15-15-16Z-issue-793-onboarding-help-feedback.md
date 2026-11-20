# Onboarding redirect, documentation link and console feedback form (issue #793)

## Added

- Two branding settings, `docs_url` and `onboarding_url` (`https://` URLs, default `none`),
  with `[auth]` defaults like `feedback_url`, set on the Settings page or with
  `okf-mcp settings set`, and recorded in the audit log.
- The console sidebar shows a "Documentation" link to `docs_url` on every page, for everyone
  signed in, by key or by account, admin or not. `GET /ui/brand.json` gains `docs_url`.
- `POST /account/setup` redirects a successful first password setup to `onboarding_url`
  (still setting the session cookie). A setup that continues an OAuth authorization resumes
  it instead. Sign-in, password reset, failed setups and passkey setup are unchanged, and
  `safe_return_to` is untouched.
- `POST /api/v1/feedback`: the console's feedback form, for a console session with
  `X-OKF-CSRF` (a key or token gets 403; no `[feedback]` table gets 404). `from` is always
  `user`, `client` is `web console` and `credential_kind` is `session`. Its hourly limit
  counts each user (`user:<users.id>`), across all their sessions.
- `GET /ui/brand.json` gains `feedback_form` (`{disclosure, max_message_chars, kinds}`, or
  `null` without `[feedback]`). The console's "Send feedback" button opens the form in the
  drawer. The button is disabled while a send runs, even in a reopened drawer, and the form
  shows the result: a confirmation, or the refusal's reason with the text kept.
- Migration `0029_feedback_from_console.sql` lets `feedback.credential_kind` be `session`.

## Changed

- The checks and queueing of feedback moved from the MCP tool into
  `mail::feedback::check_and_submit`. The `send_feedback` tool and the console's endpoint
  both call it, so they refuse and limit submissions the same way.
- The third-party disclosure sentence is one macro (`mail::feedback::third_party_disclosure!`).
  The tool's description and the console form's disclosure both use it, and the tool's
  description text is unchanged.
- With a `[feedback]` table, someone signed in with an account sees the in-console form
  instead of the external `feedback_url` link, never both. A key still sees the link, since
  the form needs a session.

## Notes

- `docs_url` and `onboarding_url` are separate settings rather than one URL plus a fixed
  path. A white-labelled server's documentation may have no getting-started page at a fixed
  path, and with separate settings an operator can show the link without the redirect.
- Both default to `none`, like the other link settings and the generic `okf` product name. A
  deployment that wants the redirect and the link sets both in `[auth]` or on the Settings
  page, for example `docs_url = "https://docs.example.com"` and
  `onboarding_url = "https://docs.example.com/start/first-15-minutes/"`.

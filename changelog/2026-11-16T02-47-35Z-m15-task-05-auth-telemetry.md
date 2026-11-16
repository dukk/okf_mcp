# M15 Task 5: sign-in, OAuth, rate-limit and mail telemetry (issue #466)

Auth, rate-limit and mail activity now shows on `/metrics` and as spans, with
labels from closed sets only.

## What changed

- `okf.auth.attempts{method,outcome}`: recorded at `signin::start_in` (Ok, after
  the session exists) and `signin::audit` (refusals), the two places that write
  a `sign_in_events` row. Labels are the audit's own `Method` and `Outcome`.
- `okf.oauth.tokens.issued{grant}` and an `okf.oauth.token` span (with
  `okf.oauth.grant`) on `POST /oauth/token`.
- `okf.ratelimit.rejections{limiter}` through the closed `Limiter` enum, at every
  refusal site: sign-in, passkeys and the OIDC start (`sign_in_per_ip`), forgot
  (`forgot_per_ip`, `forgot_per_email`), OIDC link starts, OAuth registration,
  token and revoke (`oauth_token`) and consent.
- Each mail job is a root `okf.mail.job` span (`parent = None`) with the template
  name, the provider and the outcome; `okf.mail.jobs{provider,outcome}` counts
  tries as `sent`, `retry` or `failed`. No address is in either.
- `okf.mail.queue.depth{status}`: a gauge over the counts `App::poll` stores
  (`mail::queue::record_depth`), so a scrape never queries Postgres.
- The plan has a dated correction note under Task 5.

## Why

Operators need to see sign-in failures, lockouts, rate-limit pressure and a
stuck mail queue without logs. Label values are enum spellings, so a client
cannot grow the series count, and nothing derived from an email, name, client
id, address or token is an attribute.

## How it was checked

Each new test was run with its code removed and failed. New events: one, the
poll's `counting the mail queue failed`, written with `split_event!` (error text
in the local twin).

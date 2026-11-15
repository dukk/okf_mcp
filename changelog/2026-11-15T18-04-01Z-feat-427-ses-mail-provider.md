# SES mail provider (issue #427)

## Added
- `mail.provider = "ses"`: sends each queued mail with one SES v2
  `SendEmail` through the AWS SDK (`src/mail/ses.rs`), never SES SMTP. The
  From address is `mail.from`; subject, text and HTML go as UTF-8.
- `[mail.ses]` (optional) with `region`. Unset, the SDK's region chain
  decides; with no region anywhere `serve` stops at start-up with an error
  naming `mail.ses.region` and `AWS_REGION`.
- Credentials come only from the SDK's standard chain (environment,
  profile, web identity, then the container provider EKS Pod Identity
  uses). There is no access-key setting.
- Dependencies: `aws-config` and `aws-sdk-sesv2` (no default features: no
  SSO, `credential_process` or SigV4a), and `aws-credential-types` as a
  dev-dependency for a failing credential source in tests.
- Tests against a mock SES endpoint: the signed request's shape and UTF-8
  content, permanent and retryable classification, credential failures,
  an unreachable endpoint, a stalled send, and region resolution.

## Changed
- `mail::from_config` is `async` (the SDK loads its configuration
  asynchronously); `App::load` awaits it. Only the SES branch touches the
  SDK, and enqueue-only commands still never build a mailer.
- `docs/operations.md`: the `ses` provider, its config key, retry
  classification, IAM permission, verified identity, sandbox, Pod Identity
  prerequisites and a `mail test` smoke test.

## Notes
- The SDK's own retries are disabled and each send is bounded at 30 s
  (10 s to connect): the mail queue owns retries, so an inner retry loop
  would only multiply attempts.
- Credential and authorization failures (none found, any 401 or 403,
  expired or invalid token, `AccessDenied`) are retryable, matching
  Postmark's 401 policy: an operator can fix them within the 24-hour retry
  window. Account-level
  holds (`SendingPaused`, `AccountSuspended`) are retryable too.
  `MessageRejected`, an unverified MAIL FROM domain, bad requests, a
  missing identity and other 4xx are permanent. An error SES does not
  model for `SendEmail` is sorted by its code, then by its HTTP status:
  401, 403, 408, 429 and every 5xx are retryable, so a gateway's 502 with
  no error type does not fail the mail at once (found by the claims
  audit).
- Error text carries SES's error code and message only, never credentials
  or a message body; a test checks it.
- Verified by reverting: with SDK retries on, the one-attempt test fails;
  without the operation timeout, the stalled-send test fails; without the
  status fallback, the untyped-5xx, unlisted-code and authorization tests
  fail.

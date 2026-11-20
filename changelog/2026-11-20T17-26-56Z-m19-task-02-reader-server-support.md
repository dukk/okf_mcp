## Added
- `X-OKF-Account` on every `/api/v1` answer a route gives (issue #809, M19 Task 2): `console_session::ACCOUNT_HEADER` and `account_tag`, 16 lowercase hex digits of SHA-256 of `okf-account:user:<user id>` for a console session or `okf-account:key:<key id>` for a bearer key or grant. The mobile reader's service worker will use it to keep its cached answers to one account.
- Tests: `account_tags_are_stable_opaque_and_distinct` (unit) and `api_answers_name_their_account_and_refusals_do_not` (a session's and a key's real answers carry their tags; the middleware's own 401 and CSRF 403 do not).

## Changed
- `pages::safe_return_to` also keeps same-origin `/read/...` paths, so sign-in can return to the reader. `/read`, `/readx/`, and `/read/` paths with a backslash or a space still fall back to `/ui/`.
- `docs/operations.md` names `/read/` among the allowed `return_to` prefixes and describes the new header.
- The plan's Task 2 (`docs/superpowers/plans/2026-11-20-mobile-reader-pwa.md`) has a dated correction: `account_tag` takes an `Account` enum with a prefix per kind, instead of one bare id.

## Notes
- The tag is computed before the request runs, and is inserted only on the response from `next.run`: a refusal the middleware gives itself (401, 403, 500) names no account. Answers keep every other header and status unchanged.
- A session is tagged by its user id (a UUID), never by the user name; a bearer credential by its key id. No secret is hashed, and the tag does not contain the id itself (it is a truncated hash, so it is opaque, not secret).
- User ids and key ids are separate namespaces, so each gets its own prefix: a key whose id equals a user's id still gets a different tag.

## Added
- `DELETE /api/v1/me`: a signed-in person deletes their own account, re-authenticating in the same request with `{password}` (checked as a sign-in is, counting toward the lockout) or `{passkey}`, a fresh assertion from one of their own passkeys. 204 clears the session cookie and any passkey ceremony cookie, whichever proof was used; a key or OAuth token is refused (403).
- `POST /api/v1/me/passkeys/confirm/start`: the WebAuthn challenge for that assertion. Only the account's own passkeys can answer it; the ceremony is single use, bound to the `okf_passkey` cookie and to the session. 409 for an account with no passkey.
- `users::delete_own`: the existing `users::delete`, with the API's options and the person as the actor, plus three checks under the user's row lock: the name still names the session's user, the session is still in force (held `FOR SHARE`), and the password is still the one typed or the passkey is still the account's.
- The Account page's "Delete your account" section: a current-password box and a "Confirm with a passkey" button (whichever the account has), a typed confirmation of the account ID (no control is drawn until the account read has answered), a refusal shown on the page with its reason, and the sign-in card with "Your account was deleted." afterwards.

## Changed
- `users::delete` now runs through a private `delete_checked`, which `delete_own` shares: one deletion path. Its lock order is unchanged; `delete_own` takes the session row right after the user and before any team, catalog or key.
- The console's `signedOut` takes an optional message for the sign-in card.
- `docs/operations.md`: "Deleting your own account", the two routes in "Your account API", the deletion rules and the `user.deleted` audit row. The user login spec marks "Future work" item 4 delivered.

## Notes
- Issue #355. #297 was closed by real deletion (milestone 14, PR #375: "This closes #297, by deletion rather than disabling"), and the code has no disabled state, so self-deletion deletes. When the deletion rules refuse (the last admin user, a personal team owning a catalog, the only owner of a shared team) the request is refused with that reason; the account is never disabled instead.
- An account with neither a password nor a passkey (only a linked OIDC identity) cannot re-authenticate and so cannot delete itself; the page says to set a password or add a passkey first.

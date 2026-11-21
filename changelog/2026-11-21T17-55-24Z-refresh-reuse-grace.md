# Racing refreshes by one client no longer sign it out

## Added
- `oauth.refresh_reuse_grace` (default `"60s"`, `"0s"` turns it off, at most 10 minutes). A used refresh token presented again within the window after its first use, by the same client, while its family and grant are live, is rotated again: a new pair in the same family, instead of revoking the family. The window starts at the first use and never slides; past it, a reuse revokes the family as before. Issue #884.
- `store::oauth::tokens::use_refresh` takes the window, and `RefreshUse::Rotated` says whether the rotation was a second use (`again`).
- The `okf.oauth.token` span records `okf.error` (the RFC 6749 error code, on failure) and, for a refresh, `okf.refresh` (`rotated`, `grace`, `reused` or `unknown`).
- `oauth::testing::oauth_app_with` builds the OAuth test app on a given configuration.

## Changed
- The default `oauth.access_token_ttl` is 24 hours (was 1 hour). Access tokens stay revocable at once (the access-token cache and its revocation counter), and each refresh is a chance for a client's processes to race.
- The span attribute `okf.oauth.grant` is renamed `okf.grant`. A key containing `auth` (as `oauth` does) is hidden by trace backends' default data scrubbers, so the old one arrived as `[Filtered]`.
- `docs/operations.md`: the new setting and default, a "Concurrent refreshes" section, the span reference and the troubleshooting row.

## Notes
- Cause: a client running several processes on one stored credential (Claude Code with parallel sessions and subagents) refreshes from each process when the access token expires. The first rotation won; every other presented a used token, which revoked the family, the winner's new tokens included, and the person had to sign in again. Production traces showed bursts of `okf.oauth.token` spans within one second, one `ok` and the rest `failed`, about once an access-token lifetime.
- Each process that loses the race gets a pair of its own, so a family can hold several live refresh tokens, each rotating on its own. Revoking the family still revokes all of them.
- A used token in a revoked family, or of a revoked or expired grant, is still `Reused` within the window (a family revocation revokes used tokens too), so the window never revives an ended connection. A different client presenting the token is refused without spending anything, as for an unused token.
- The window does not help a process that keeps a stale refresh token for longer than the window and presents it later; the longer access-token lifetime makes such refreshes rarer.

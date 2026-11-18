## Added
- Tests pinning both branches of a CIMD refetch with a cached copy: an invalid document (changed `client_id`, empty or unsafe `redirect_uris`, not JSON, over the size cap) fails the authorization and leaves the cached row untouched; a transport failure (HTTP 500, refused connection) serves the cached copy within the 24-hour ceiling and fails past it.
- `CimdError::allows_stale_fallback`, the one place that says which fetch failures may use the cache.

## Changed
- A refetch that delivers a document that is not JSON or is over the size cap now fails the authorization instead of serving the cached copy. Before, every fetch error fell back to the cache; only validation failures failed closed.
- The OAuth design spec's "Client ID Metadata Documents" section and docs/operations.md state the rule.

## Notes
- Decision from issue #268: data that arrived without a transport error but is invalid means the authorization fails. Transport failures (no DNS answer, a refused non-public address, timeout, connection or TLS failure, any non-200 status) still serve the cache for up to 24 hours after it was fetched.
- After an invalid refetch the cached row is kept as it was (not replaced, not evicted); the next valid fetch refreshes it. While it is expired, every authorization refetches.
- The 5-second timeout case is covered by the classification table test, not an end-to-end test, to avoid a 5-second wait.

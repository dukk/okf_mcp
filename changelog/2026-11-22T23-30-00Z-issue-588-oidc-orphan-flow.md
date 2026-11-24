# Issue 588: remove the stored OIDC flow when the link index swap fails

With `[redis]` on, `OidcProviders::store_flow` inserts the new flow and then
swaps the console session's link index to point at it. When the swap failed
(a Redis error), the `?` returned early and the flow stayed in Redis until its
10-minute TTL. The start already failed closed (503) and the flow cannot be
finished without its cookie, so this was untidy rather than unsafe, but it held
Redis memory.

## Change

- `src/oidc.rs`: on a failed `Index::replace`, `store_flow` removes the flow it
  just inserted (best effort) and returns the swap's own error. A failed
  removal is already counted as a Redis `remove` error by the store, so no new
  metric is needed.
- `src/http/oidc.rs`: `a_failed_link_index_swap_removes_the_stored_flow` (on a
  standalone Redis and a one-node cluster) restricts the default user's keys
  so the link index's `SET` gets `NOPERM`, starts a link, and asserts a 503 and
  that no `oidc:flow` key remains. Without the fix the flow key is left behind.

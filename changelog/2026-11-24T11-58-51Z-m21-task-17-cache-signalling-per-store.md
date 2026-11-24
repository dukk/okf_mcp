# M21 Task 17: cache signalling per store

Issue #975, Task 17 of `docs/superpowers/plans/2026-11-23-okf-mcp-catalog-stores.md`. It completes release 3.

## Added

- A change-feed listener per opened store (`invalidate::spawn_store_feed`, `Stores::follow`). `serve` calls
  `follow` once; from then on opening a store's pool starts a listener on that store's database, held in the
  registry entry, so forgetting the store (or every store) or the shutdown ends it and closes its connection.
  A store inside the account database gets none (the account listener hears it).
- `App::apply_store_reconnect` and `CatalogDirectory::drop_store`: a store listener that reconnects, or reads an
  unreadable payload, drops the cached rows of that store's catalogs only.
- Tests in `src/invalidate/store_feed_tests.rs`: `a_publish_on_a_separate_store_reaches_the_other_replica`,
  `a_lost_store_listener_drops_only_that_stores_catalogs`, `forgetting_a_store_ends_its_listener_and_closes_its_connection`,
  `the_shutdown_ends_a_store_listener`, `a_store_that_cannot_be_listened_to_does_not_hold_up_another`,
  `a_store_in_the_account_database_gets_no_listener_of_its_own`.

## Changed

- `publish`, `import` and `repair` send `Invalidation::Catalog` on the store transaction instead of the account
  transaction. It is delivered when the generation commits, with no window in which the store has committed and
  the message is lost because the account commit failed. Other invalidations are unchanged.
- The account listener's loop is now `invalidate::Feed::run`, shared with the store listeners: same backoff,
  catch-up and shutdown behaviour.
- `invalidate::testing::messages_of` also listens on the store `main` when it is another database.
- `docs/operations.md`: each opened separate store holds one more connection (its listener), outside
  `store_max_connections`; the connection arithmetic says so.
- Plan Task 17 is corrected.

## Why

A store on its own database sends its catalog messages on that database. Without a listener there no replica
would hear them, and release 3's maintenance-window upgrade (P10) would leave other replicas' caches to the TTL.

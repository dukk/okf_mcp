# M21 Task 8: `Stores`, the registry in code

## Added
- `src/stores/registry.rs`: `Stores`, the pools of the catalog stores, opened on first use from the `stores` row with their own `search_path` (`STORE_SEARCH_PATH`, `public` until Task 11), size, idle timeout and connect timeout. Callers that ask for one store together share one open; a failed open is not remembered; `StoreUnavailable` after `store_connect_timeout`, never naming the store or host. `forget` drops a store's pool (users of the old handle finish on it); `forget_all` drops every one.
- `src/store/stores.rs`: the SQL for `stores` (`by_name`, `names`).
- Config keys `store_max_connections` (8), `store_idle_timeout` (`10m`), `store_connect_timeout` (`5s`), in `docs/operations.md`.
- `Invalidation::Store { name }`: each replica forgets that store's pool; a change-feed reconnect forgets them all.
- `App::stores()`, `App::catalog_db(&CatalogRow)`, `App::store_db(&StoreName)`; `CatalogState::store()`; `CatalogRow::store`.

## Changed
- `App::main_store()` is gone. Callers use the store of the catalog they act on. `CatalogCache` reads each catalog through the registry.
- The directory queries hide catalogs whose `provisioned_at` is NULL.
- CLI `import` and `export` open the catalog's store through the registry; `repair-indexes` opens `main` through it.
- Tests: `an_unprovisioned_row_is_hidden_from_lookups_and_listings`, `account_database_store_opens_its_own_pool`, `concurrent_first_use_opens_one_pool`, `an_unreachable_store_is_store_unavailable_within_the_timeout`, `changing_a_store_row_reopens_its_pool_on_every_replica`, and others beside them.

## Notes
- Still on `main` for now, marked in the code: the changeset list across catalogs and a changeset handle's lookup (Task 18), `prune_changesets` and the open-changeset count (Task 16), CLI `repair-indexes` (Task 20). Nothing writes a `stores` row yet (Task 10 adds the admin surface and refuses host stores until Task 20, P8), so none of these can miss a catalog yet.
- Only `env` credentials open a `host` store; the user, TLS settings and other connection options are the account pool's. Task 9 adds the other kinds.
- No crossings were added to or removed from the release-1 inventory: the registry opens pools only on the store side.

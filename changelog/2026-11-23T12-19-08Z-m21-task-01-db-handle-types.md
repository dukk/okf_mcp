# M21 Task 1: database handle types

## Added
- `src/stores/mod.rs`: `AccountDb` and `CatalogDb`, two wrappers around a `PgPool` so a function that takes one cannot be given the other, and `StoreName`, a validated store name (`^[a-z0-9-]+$`, at most 63 characters).
- `App::accounts()` and a temporary `App::main_store()` (store `main`, the same pool as the account database in release 1).
- `OkfError::StoreUnavailable { store }`, answered 503 over HTTP. Its message says only that the catalog's data is temporarily unavailable. `OkfError::store_unavailable` logs the store name in the local half of a `split_event!`, never in the response or OTLP.

## Changed
- `StoreName` deserializes through `StoreName::parse` (`#[serde(try_from = "String")]`), so an invalid name cannot be built from JSON either (review of PR #981).
- `App::pool()` is `#[deprecated]`; `#![allow(deprecated)]` in `src/lib.rs` keeps the crate building until Task 6 removes both.
- The two browser end-to-end tests that called `app.pool()` use `app.accounts().executor()`, since they are a separate crate the crate-wide allow does not cover.
- The plan notes the shipped shapes (`Display`, `pub(crate)` constructor, `main_store()` by reference).

## Notes
- No call site is migrated; Tasks 2 to 6 do that. `Stores` is Task 8.
- Tests: `store_name_parse_accepts_slugs_and_refuses_the_rest`, `main_store_and_accounts_reach_the_same_database_in_release_one`, `store_unavailable_is_503_and_hides_the_store_name`.

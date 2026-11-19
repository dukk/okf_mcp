# M18 Task 11: credential and user caches replace the key cache

Issue #689. Plan: `docs/superpowers/plans/2026-11-18-okf-mcp-m18-bounded-memory.md`, Task 11 (spec Decision 9).

## Added

- `src/cache/credentials.rs`: `Credentials`, three bounded caches (API keys by SHA-256, OAuth grants by id, users by id). A principal is assembled per request from the credential entry and its user's entry; a self-issued credential is capped by the user's current ceiling. Unknown, revoked and expired credentials and unknown users are never cached.
- `Credentials::apply`: `credential` drops the key or grant (by id, and a fenced scan by hash); `user` drops the user; `team` drops every cached member (fenced scan); `catalog_meta` drops every cached credential and user whose reach, default or ceiling names the catalog, so a grant of a catalog deleted and created again under its name never reaches the new one. `mark_all_stale` on a lost listener connection.
- `Bounded::get_or_load_present`: single-flight loads whose "nothing found" is shared but not cached.
- Store reads by one id: `keys::record_by_hash` and `record_by_id` (with the audit credential id, one statement), `gates::for_user` (`UserGates`: team ids, entitled catalog ids, live unlocks by catalog id, admin flag, audit person id) and `gates::audit_credential_id`.
- `AccessFacts` on every `Principal`, and `App::find_key`, `find_grant`, `session_principal`, `access_view`, `labels`, `apply_m16_announcement`.
- Config: `credential_ttl` (10 s), `credential_cache_entries` and `user_cache_entries` (100,000 each). Metrics: `okf.cache.*` with `cache="credential"`, `"grant"` and `"user"`.

## Changed

- Suspension, maintenance opt-out, ownership and audit team are read from the catalog directory's row at the moment of each check: `Principal::require`, `check_open`, `admission`, `require_contents`, `require_maintenance_report` and `suspension` take a `&CatalogRow`. Memberships, entitlements and unlocks match the row's ids, not its name.
- `ToolContext` resolves the directory row of the very incarnation it reads (`row`), and refuses `Busy` when the name has become another catalog; `admit`, `admit_all` and `check_contents` are async. The selection resolves the rows of its candidate names.
- Console sessions and OAuth grants load on a miss: the database fallback in `session_user`/`session_principal` and the catch-up retries in `oauth::access` and the refresh grant are gone.
- `reload_keys_now` is `reload_settings_now`: settings and labels only (labels stay a full load until Task 12). The poll pass marks the credential caches and the directory stale when it sees `admin_generation` move ahead of the change messages, until Task 16 removes the poll.

## Removed

- `Keys`, `KeyCache`, `AccessGates`, `Suspensions`, `App::keys`, `App::catch_up_keys`, `gates::load`, `ceiling::ceilings`, `teams::suspended_catalogs`, `users::auth_by_id`.

## Notes

- Labels are still loaded whole on every `admin_generation` change (as before) until Task 12; `Principal::suspension` reads them from `AccessFacts`.
- A revoked credential stops on every replica when its `credential` message is applied; a load that started before the revocation serves its own request only (the cache's fence). `credential_ttl` bounds a lost message.

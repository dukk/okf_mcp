# M18 Task 19: a user's ceiling is resolved per catalog (issue #728)

The milestone 18 scale test found one path whose cost still grew with the
number of catalogs: authenticating a user read their whole ceiling (one row
per catalog their teams own or they are entitled to), and the user cache
kept it. A user whose team owns a million catalogs took about 820 ms to
authenticate and held a million-entry map in one cache entry.

## Added

- `store::ceiling::level_on(row, gates)`: the ceiling on one catalog, from
  the catalog's directory row (owner team, `member_access`) and the user's
  gates (teams, entitlements with their levels). `ceiling_over` applies it
  to a set of rows; `highest` answers whether any ceiling catalog is at
  `write` (what `GET /whoami` shows as a session's `access`).
- `Reach::Ceiling`: a non-admin console session's reach, resolved per
  catalog. Row-based checks on `Principal`: `level_at`, `reaches_row`,
  `require_level_at`, `precheck_level`, `excludes`.
- `store::catalogs::ceiling_page`: a session's catalogs a page at a time,
  per owner team, over the new partial index `catalogs_member_reach`
  (migration `0025_catalogs_member_reach.sql`).
- `CatalogRow::member_access`; `UserGates::entitled` carries each
  entitlement's level.
- Tests: `the_per_catalog_ceiling_answers_what_the_whole_ceiling_did` (old
  whole-ceiling answer against the per-catalog one, for every source of a
  ceiling, an admin user and `Reach::All`) and
  `a_user_whose_team_owns_many_catalogs_loads_a_small_entry` (50,000
  catalogs: the user load returns a handful of rows; the entry is small).

## Changed

- `UserEntry` no longer holds a ceiling, and the user load no longer reads
  one: its queries are bounded by the user's teams, entitlements and
  unlocks. A `catalog_meta` message no longer drops cached users.
- A self-issued key or grant is capped by the ceiling on its own catalogs,
  resolved from their directory rows on each assembly (`Credentials` holds
  the application's `Arc<CatalogDirectory>`).
- Key creation (`POST /me/keys`) and OAuth grant creation read the ceiling
  of the named catalogs only (`ceiling_in(conn, user_id, names)`).
- `GET /session` reads the user's ceiling when asked (it was the cached
  one); `GET /me` and the consent page already did.
- `ToolContext::offers` takes a catalog row; the listing, selection and
  changeset paths check reach on rows.
- `tests/scale.rs`: the million catalogs belong to the administrator's team
  again, the administrator's first authentication is measured and bounded,
  and the measured app's `credential_ttl` is one hour so the credential
  entry limit, not expiry, holds the count.

## Notes

- Name-only checks (`level`, `reaches`, `require_level`) fail closed for a
  ceiling; every path a console session reaches uses the row.
- The single-catalog selection fallback does not apply to a session (every
  HTTP route a session calls names its catalog).
- `GET /me`, `GET /session` and the consent page still show the whole
  ceiling, read from the database on request; their answers grow with it.
  Paging them is a follow-up (#736).
- Scale run: A release run of the reworked test (2026-11-19, alone on the machine): the administrator's first authentication 3.20 ms (15 µs again), against about 820 ms before; 10,000 keys p50 0.34 ms, p99 0.55 ms; credential entries 5,000 at the 5,000 limit with `credential_ttl` 1 h; first listing page 18.65 ms, repeat 0.94 ms.

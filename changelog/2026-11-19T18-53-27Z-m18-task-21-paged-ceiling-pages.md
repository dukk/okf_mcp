# M18 Task 21: `/me`, `/session` and the consent page page the ceiling (issue #736)

`GET /api/v1/me`, `GET /api/v1/session` and the OAuth consent page read a
user's whole ceiling (one row per catalog the user's teams own or they are
entitled to) and returned or rendered it whole, so their answers, and a
consent's pending authorization, grew with the catalogs a team owns. No
request reads or returns the whole ceiling now.

## Added

- `store::ceiling::page`: one page of a user's ceiling, by name prefix and
  cursor, in byte order, one index range scan per owner team.
  `store::ceiling::top_level`: the ceiling's highest level, or none.
- Migration `0027_catalogs_member_reach_bytes.sql`: the partial index
  `catalogs (owner_team_id, name COLLATE "C") WHERE member_access <> 'none'`,
  built concurrently (and dropped and rebuilt by `db::migrate` if a build
  failed part-way).
- `GET /api/v1/me/ceiling?prefix=&after=&limit=`: `{ceiling: [{catalog,
  access}], next}`.
- The consent page's Find (catalogs whose name starts with what is typed)
  and Next catalogs, which carry the ticked catalogs over.
- `src/http/ui/js/components/ceiling.js`: a paged ceiling with a Find box
  and Load more, for the Account page and New key's picker.
- `rows_returned::counting`: runs a future and counts the rows its queries
  returned.

## Changed

- `GET /api/v1/me`: `ceiling` is gone; `ceiling_access` (`read`, `write`,
  `null`) names the highest level.
- `GET /api/v1/session`: `ceiling` is gone.
- `GET /api/v1/whoami`: `levels`, the level on each catalog of the page.
- `GET /api/v1/me/keys` and `/me/grants`: `ceiling`, on their live
  self-issued credentials' catalogs only.
- The consent page offers 50 catalogs at a time; a pending authorization
  holds at most 100 offered catalogs. A lone catalog starts preselected
  only when it is the whole ceiling.
- The console reads a member's levels from `whoami`'s `levels`; My keys and
  My grants read the cap notes' ceiling from their own answers.
- `ceiling::ceiling_of` is test-only.
- New key's picker lowers a ticked catalog's chosen level when the ceiling
  has lowered it since (the map and the select stay in step), so Create
  never sends a level the ceiling no longer allows (review of PR #743).

## Notes

- Clients must change: read `GET /me/ceiling` page by page instead of
  `/me`'s or `/session`'s `ceiling`, or `whoami`'s `levels` for the catalogs
  it lists (`docs/operations.md`, Upgrades).
- The deciding check for a consent is unchanged: `grants::create_self_issued`
  re-checks the chosen catalogs against the ceiling under the user's lock.
  A Find or Next re-reads the carried catalogs' ceiling too, so a lowered
  level is offered lowered.

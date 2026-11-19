# M18 task 12: labels on demand

## Added
- `cache::labels::LabelLookup`: one bounded cache (100,000 entries, `credential_ttl` backstop) of user, team and catalog labels by slug. `labels_for(&Wanted)` returns exactly the slugs asked for, reading every miss together in one query per kind; `labels_with_owners` adds the owner teams of the catalogs.
- `Bounded::get_or_load_many`: a batch load whose values are kept per key unless an invalidation overtook the read.
- `SuspendedRow::team_label`, computed by the directory query, so a suspension names its team without a label lookup.

## Changed
- `Labels` is a view over the slugs one response shows. `App::labels()` is the lookup; the console-facing endpoints (labels, catalogs, identity, audit, teams, archives export, MCP tools, OAuth consent pages) build a `Wanted` from what they render. `ctx.catalog_label` is async.
- `AccessFacts::labels` and `AccessView::labels` are gone; `Principal::suspension` reads both labels from the catalog row. The directory drops a suspended row on a `Team` message for its owner and on any `User` message.
- Label messages: `User` drops that user's label and its personal team's, `Team` that team's, `CatalogMeta` that catalog's. Reconnect, an M16 announcement and the poll's sweep mark every label stale.
- Labels are no longer loaded whole on an `admin_generation` change (the temporary regression Task 11 named).

## Notes
- Labels are display names: no payload, log line or metric label carries one, and a response gets only the slugs it asked for. `GET /labels` for an every-catalog admin still returns every user and team, read whole and uncached (as before; it is not paged).
- `Labels::load` and `teams_of` are removed; the CLI audit commands build a throwaway lookup.

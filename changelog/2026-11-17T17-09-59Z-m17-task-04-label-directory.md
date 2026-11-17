# M17 Task 4: the label directory in the key cache

## Added
- `src/labels/directory.rs`: `Labels`, every user's, team's and catalog's label by slug, plus memberships and each catalog's stored display name and owner team. `Labels::load` reads all four tables at once; `from_rows` builds it from rows. Accessors: `user`, `team`, `catalog`, `catalog_display_name` (the stored value, empty when unset), `actor` (resolves only a user slug), `catalog_owner`, `teams_of` and `catalog_choices`.
- `Labels::catalog_choices` tells duplicate catalog labels apart with the owner team's label, then moves every entry still colliding (including a literal label shaped like a fallback) to `{label} (id: {slug})`, repeating until the whole final set is distinct.
- `KeyCache` holds the directory (`with_labels`), `Keys::labels()` exposes it. It loads as a fifth `try_join!` arm of `KeyCache::read`, so it reloads on every `admin_generation` bump on every replica, the multi-replica path included.
- `SuspendedCatalog` gains `catalog_label` and `team_label`, filled from the directory in `KeyCache::read` (the slugs stand in if a row is missing, a race).

## Tests
- Unit tests for the label rule, actor resolution, duplicate handling (owner suffix, same-owner and shared-owner-label fallbacks, a literal label shaped like a fallback), a hegel property that choices are always distinct, the raw display name, and `teams_of`.
- Key-cache tests: after a user and catalog are created and the cache reloads, their labels are present; suspension notices carry the directory's labels.

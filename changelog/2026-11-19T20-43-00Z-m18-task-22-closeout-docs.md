# M18 task 22: close-out documentation gaps (issue #745)

Docs only. Closes the gaps G1-G6 of the pre-merge audit of epic #678.

## Spec (`docs/superpowers/specs/2026-11-18-okf-bounded-memory-design.md`)

Dated `Corrected (issue #745, 2026-11-19)` notes describing what shipped:

- Decision 2: the catalog cache has no TTL of its own (#686).
- Decision 8: the fan-out cap applies only when no catalogs are named (#725).
- Decision 9: the user cache holds `{ auth, gates }`; the ceiling is resolved per catalog (#728, PR #739); Task 21 response shapes (#736: `/me/ceiling`, `ceiling_access`, `whoami` `levels`).
- Decision 14: the bridge payload is `"notified"` (#691).
- Decision 17: `prune_workers` keeps any worker row a job names (#692).
- Decision 20: a missing audit row is recorded with no credential id, not refused with 503; backfilled by migration 0023. The maintainer kept this (2026-11-19).
- Testing: the hegel property test is deferred to #744; the scale test used a small budget.

## Milestone changelog

Added sync PRs #733 and #742, follow-ups #734, #738, #741 and #744, the Task 21 response shapes under Changed, `okf_housekeeping_duration_seconds` under Metrics added, every plan `Corrected` note under Plan divergences, and a note that the default settings were kept.

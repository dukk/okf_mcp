# Bounded-memory design (milestone 18)

Added `docs/superpowers/specs/2026-11-18-okf-bounded-memory-design.md`.

- Three replicas multiplied poll passes, which showed that every replica
  holds every catalog's snapshot, the whole key cache, and per-catalog
  metric series, and runs cluster-wide housekeeping.
- The design replaces these with bounded on-demand caches (catalogs,
  catalog directory, credentials, users, labels), targeted Redis
  invalidations with a 10s TTL backstop, paginated catalog listing, a
  search fan-out cap, and housekeeping as apalis jobs (folding in #458).
- The poll loop is removed. No code changes yet.

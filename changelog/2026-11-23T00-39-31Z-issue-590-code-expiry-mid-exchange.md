# Issue #590: a code expiring mid-exchange is not a replay

## Added
- `Ephemeral::update_with_ttl_floor`: `update`, and in Redis the entry
  then lives at least the given floor longer, set by the same compare-and-set
  script that writes the change. A zero floor is `update`.
- Tests: a code whose lifetime ends while its exchange is minting still
  succeeds and keeps its tokens (standalone and cluster Redis); a genuine
  replay across replicas still answers `invalid_grant` and revokes the
  family; the floor lengthens but never shortens or revives an entry.

## Changed
- The authorization-code exchange marks the code used with a floor
  (`USED_CODE_FLOOR`: the 120 s request timeout plus 30 s, so it covers
  any exchange the server lets run). Before, a code used in the last milliseconds of its
  60 s life could be gone by the post-commit replay check, which treats a
  missing code as a replay: it revoked the legitimate tokens and answered
  "the code was used twice".

## Notes
- Chosen over telling "missing" from "replayed": a missing code still cannot
  vouch for itself (Redis could have lost it), so it must keep revoking;
  keeping the spent code until the check closes the window instead.
- An unused code is never extended: the floor is applied only by the write
  that marks it used, atomically in the script. A code that vanishes before
  the mark still answers "unknown or expired code" with nothing minted.
- Replay detection is unchanged: a code presented twice finds `used`, sets
  `replayed`, and revokes the family. The memory backend is unchanged (it
  judges liveness by the exchange's start instant).

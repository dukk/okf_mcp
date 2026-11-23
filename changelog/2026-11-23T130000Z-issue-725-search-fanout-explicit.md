# Issue 725: the search fan-out cap covers an explicit catalog list

Date: 2026-11-23

## Problem

`search_fanout_max` applied only to a `GET /api/v1/search` that named no
catalogs. A request listing its catalogs was not capped, and the console listed
every reachable catalog, so one console search could load and scan them all.

## Change

- `check_fanout` (`src/mcp/search.rs`) refuses a search naming more than
  `search_fanout_max` distinct catalogs with `InvalidPath` on `catalogs` (400).
  `run_with` calls it, so `GET /api/v1/search` and the MCP `search` tool share it.
- `GET /whoami` returns `search_fanout_max`.
- The console search page (`src/http/ui/js/pages/search.js`) names no catalogs
  when the key reaches at most the cap, so the server's every-reachable path
  applies. Past the cap it shows a checkbox chooser that stops at the cap and
  says why. Its read goes through `core/latest.js`.
- The spec (Decision 8), `docs/operations.md` and the README are corrected.

## Not done

The chosen catalogs are kept for the session only, not stored in the URL.

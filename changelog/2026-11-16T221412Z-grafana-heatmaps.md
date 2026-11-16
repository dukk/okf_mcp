# Grafana dashboard: every metric, histograms as heatmaps

## Changed
- `examples/grafana/okf-mcp.json` now has a panel for all 25 families in `Metrics::ALL_FAMILIES` (it showed 10), in nine rows: overview (version, request rate, 5xx ratio, MCP sessions, DB connections in use, clock offset), HTTP, MCP tools, publish, catalogs and polling, auth and rate limits, mail, archives, platform.
- The six histograms (`http_server_request_duration_seconds`, `okf_mcp_tool_duration_seconds`, `okf_publish_duration_seconds`, `okf_publish_lock_wait_seconds`, `okf_poll_duration_seconds`, `okf_archive_duration_seconds`) are heatmaps: `sum by (le) (increase(<family>_bucket{...}[$__rate_interval]))` with `format: heatmap` and `calculate: false`, so Grafana draws the server's own `SECONDS_BUCKETS` rather than re-bucketing. Quantile lines (p50/p95/p99, or p95 per route or tool) sit beside them.
- New template variables `catalog` (from `okf_catalog_concepts`, all = `.*`, which also matches tool calls with `catalog="none"`) and `tool` (from `okf_mcp_tool_duration_seconds_count`). Every selector is still scoped by `$job`.
- The dashboard targets Grafana 10.4+ (`schemaVersion` 39, the current heatmap panel).

## Tests
- `tests/grafana_dashboard.rs`: the dashboard must use every family in `Metrics::ALL_FAMILIES` (not the eight it checked before), so a new instrument fails the test until it has a panel; and any panel that reads raw `_bucket` series outside `histogram_quantile` must be a non-calculating heatmap with `format: heatmap`, with all six histograms covered.

## Notes
- `docs/operations.md` describes the rows, the variables and the new checks.

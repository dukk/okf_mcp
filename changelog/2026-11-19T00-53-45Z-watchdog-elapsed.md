# watchdog.sh no longer alarms on a bogus elapsed time

## Added
- `agentic_uptime` in `tools/agentic/lib.sh`: the host uptime in whole seconds (`$AGENTIC_UPTIME_FILE`, default `/proc/uptime`).
- Watchdog tests with a stub `ps`: a wrapped `etimes` reading is ignored, a process gone on re-read is ignored, an implausible re-read is ignored, and a genuinely long process still exits 3.

## Changed
- `tools/agentic/watchdog.sh` ignores a reading whose elapsed time exceeds the uptime, and re-reads a flagged pid once (`ps -o etimes= -p <pid>`, bounded through `agentic_bounded_call`). It alarms only when the process still exists and is still over its limit with a plausible value. The output format is unchanged.
- `tools/agentic/README.md` describes the alarm rule.

## Notes
- Issues #371 and #534 (duplicates): `ps` reported `elapsed=4123168608s` (2^32 - 171798688, a slightly negative age wrapping) for processes that were starting or exiting.
- If the uptime is unreadable, the plausibility cap is skipped; the re-read still applies.

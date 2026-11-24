#!/usr/bin/env bash
# expect.sh start <worktree> [label] | done <worktree> | list
#
# Coordinator-side ledger of reports the coordinator is waiting for (#850).
# `start` records that an agent in <worktree> owes a report: run it when you
# start the agent and again whenever you send it back (review comments). `done`
# clears it when the report arrives. watchdog.sh exits 3 when a pending
# expectation's worktree has gone idle, so an agent that stopped without a
# report does not leave the coordinator waiting for hours.
# The ledger is one file per worktree, named by the worktree's basename, under
# <git-common-dir>/agentic-expect/, shared by every worktree. Each holds
# `start=<epoch>`, `path=<worktree>` and `label=<one line>`. `start` again on
# the same worktree replaces the entry and restarts its quiet clock.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
source "$here/lib.sh"

usage() { echo "usage: expect.sh start <worktree> [label] | done <worktree> | list" >&2; exit 2; }

# The absolute, symlink-free path of a worktree directory, or exit 2.
resolve() {
    [ -d "${1:-}" ] || { echo "expect.sh: no such directory: ${1:-}" >&2; exit 2; }
    (cd "$1" && pwd -P)
}

command=${1:-}
dir=$(agentic_expect_dir)
case "$command" in
    start)
        wt=$(resolve "${2:-}")
        # One line, no control characters, so the file stays parseable.
        label=$(printf '%s' "${3:-}" | tr -d '\000-\037')
        mkdir -p "$dir"
        tmp=$(mktemp "$dir/.new.XXXXXX")
        printf 'start=%s\npath=%s\nlabel=%s\n' "$(agentic_now)" "$wt" "$label" >"$tmp"
        mv "$tmp" "$dir/$(basename "$wt")"
        echo "expecting a report from $(basename "$wt")"
        ;;
    done)
        wt=$(resolve "${2:-}")
        if rm -f "$dir/$(basename "$wt")"; then
            echo "cleared the expectation for $(basename "$wt")"
        fi
        ;;
    list)
        now=$(agentic_now)
        for file in "$dir"/*; do
            [ -f "$file" ] || continue
            started=$(agentic_expect_field "$file" start)
            case "$started" in '' | *[!0-9]*) started=$now ;; esac
            echo "$(basename "$file") waiting $((now - started))s: $(agentic_expect_field "$file" label)"
        done
        ;;
    *) usage ;;
esac

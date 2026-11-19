#!/usr/bin/env bash
# watchdog.sh [--once]
#
# Coordinator-side hang detector. Run it in the background for the whole epic.
# Every 5 minutes it looks for test or gate processes whose working directory
# is inside this repository (the main checkout or any registered worktree,
# wherever it lives), and exits 3, which
# notifies the coordinator, when one has run longer than its limit:
#   test binaries (target/.../debug/deps/*)   AGENTIC_WATCHDOG_TEST   default 1200 s
#   cargo test|clippy|build, just test|lint   AGENTIC_WATCHDOG_GATE   default 4800 s
# A reading is only trusted when it is plausible: ps sometimes reports a
# wrapped etimes (about 4.1e9 s) for a process that is starting or exiting
# (#371, #534). So a reading above the host's uptime is ignored, and a flagged
# process is read once more (bounded) and must still exist, still be over its
# limit and still be no older than the host.
# It reports each process's pid, elapsed time, directory and command line.
# After handling an alert, start it again. With --once it checks a single time
# and exits 0 when nothing is over its limit.
#
# It catches what a PR-review watcher cannot: an agent stuck before it has
# opened a PR. (A timeout-less race test once stalled a task for 12 hours.)
set -u
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
source "$here/lib.sh"

root=$(agentic_main_checkout)
once=0
[ "${1:-}" = --once ] && once=1
test_limit=${AGENTIC_WATCHDOG_TEST:-1200}
gate_limit=${AGENTIC_WATCHDOG_GATE:-4800}
interval=${AGENTIC_WATCHDOG_INTERVAL:-300}

# True when $1 is one of the checkouts or below one, on a directory boundary
# (so /x/wt2 is not inside /x/wt).
in_checkout() {
    local path
    for path in "${checkouts[@]}"; do
        case "$1" in "$path" | "$path"/*) return 0 ;; esac
    done
    return 1
}

# True when $1 (seconds) is no more than the host's uptime. With the uptime
# unknown, nothing is ruled out.
plausible() {
    [ -z "${uptime:-}" ] || [ "$1" -le "$uptime" ]
}

while :; do
    # Re-read every round: worktrees come and go during an epic. The call is
    # bounded (lib.sh: agentic_git), so a stuck git can't stall the watchdog.
    checkouts=("$root")
    while read -r path; do checkouts+=("$path"); done \
        < <(agentic_git -C "$root" worktree list --porcelain | sed -n 's/^worktree //p')
    found=""
    uptime=$(agentic_uptime)
    while read -r pid elapsed args; do
        cwd=$(readlink "/proc/$pid/cwd" 2>/dev/null) || continue
        in_checkout "$cwd" || continue
        limit=$gate_limit
        case "$args" in */debug/deps/*) limit=$test_limit ;; esac
        if [ "$elapsed" -gt "$limit" ]; then
            plausible "$elapsed" || continue
            # Re-read: the first reading may belong to a process that has gone.
            again=$(agentic_bounded_call ps -o etimes= -p "$pid" 2>/dev/null) || continue
            again=${again//[[:space:]]/}
            case "$again" in '' | *[!0-9]*) continue ;; esac
            [ "$again" -gt "$limit" ] && plausible "$again" || continue
            found+="pid=$pid elapsed=${elapsed}s cwd=$cwd cmd=${args:0:200}"$'\n'
        fi
    done < <(ps -eo pid=,etimes=,args= \
        | grep -E '/debug/deps/|cargo (test|clippy|build)|just (test|lint|test-ui)' \
        | grep -v -E 'grep|watchdog\.sh')
    if [ -n "$found" ]; then
        echo "WATCHDOG $(date -u +%FT%TZ): long-running test processes in $root:"
        printf '%s' "$found"
        exit 3
    fi
    [ "$once" = 1 ] && exit 0
    sleep "$interval"
done

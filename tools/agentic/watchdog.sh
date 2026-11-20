#!/usr/bin/env bash
# watchdog.sh [--once]
#
# Coordinator-side hang detector. Run it in the background for the whole epic.
# Every 5 minutes it looks for test or gate processes, and idle stdin readers,
# whose working directory is inside this repository (the main checkout or any
# registered worktree, wherever it lives), and exits 3, which notifies the
# coordinator, when one has run longer than its limit:
#   test binaries                             AGENTIC_WATCHDOG_TEST   default 1200 s
#     (.../debug/deps/<name>-<hash>, or .../debug/build/<package>/<hash>/out/
#     <name>-<hash> in the newer build-dir layout, which cargo 1.100 uses)
#   cargo test|clippy|build, just test|lint   AGENTIC_WATCHDOG_GATE   default 4800 s
#   idle stdin reader                         AGENTIC_WATCHDOG_IDLE   default 600 s
#     A cat, tee, head or tail (without -f) whose fd 0 is a pipe or socket and
#     which has used at most one second of CPU. A background command's stdin
#     never closes, so a stray `cat` there hangs forever with no output and
#     the gate behind it never starts (#780). A bash blocked in `read` is not
#     checked: /proc cannot tell it from a bash idling for another reason (a
#     shell waiting for commands on its stdin looks the same), so flagging it
#     would raise false alarms. sleep, `gh pr checks --watch` and wait loops
#     don't read stdin and are not flagged.
# A reading is only trusted when it is plausible: ps sometimes reports a
# wrapped etimes (about 4.1e9 s) for a process that is starting or exiting
# (#371, #534). So a reading above the host's uptime is ignored, and a flagged
# process is read once more (bounded) and must still exist, still be over its
# limit and still be no older than the host.
# It reports each process's pid, elapsed time, comm (the executable's name)
# and directory, never its command line: command lines can carry credentials.
# (Matching still reads the command line internally.)
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
idle_limit=${AGENTIC_WATCHDOG_IDLE:-600}
interval=${AGENTIC_WATCHDOG_INTERVAL:-300}
# Up to this much CPU (utime + stime, in clock ticks; default one second) still
# counts as idle.
idle_cpu_ticks=${AGENTIC_WATCHDOG_IDLE_CPU:-$(getconf CLK_TCK 2>/dev/null || echo 100)}

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
        case "$args" in */debug/deps/* | */debug/build/*/out/*) limit=$test_limit ;; esac
        if [ "$elapsed" -gt "$limit" ]; then
            plausible "$elapsed" || continue
            # Re-read: the first reading may belong to a process that has gone.
            again=$(agentic_bounded_call ps -o etimes= -p "$pid" 2>/dev/null) || continue
            again=${again//[[:space:]]/}
            case "$again" in '' | *[!0-9]*) continue ;; esac
            [ "$again" -gt "$limit" ] && plausible "$again" || continue
            read -r comm <"/proc/$pid/comm" 2>/dev/null || comm=unknown
            found+="pid=$pid elapsed=${elapsed}s comm=$comm cwd=$cwd"$'\n'
        fi
    done < <(ps -eo pid=,etimes=,args= \
        | grep -E '/debug/deps/|/debug/build/[^/ ]+/[^/ ]+/out/|cargo (test|clippy|build)|just (test|lint|test-ui)' \
        | grep -v -E 'grep|watchdog\.sh')
    # Idle stdin readers (#780). The list is by comm only; the command line is
    # read here just to tell `tail -f` (a legitimate follower) from `tail`.
    while read -r pid elapsed comm; do
        [ "$elapsed" -gt "$idle_limit" ] || continue
        plausible "$elapsed" || continue
        cwd=$(readlink "/proc/$pid/cwd" 2>/dev/null) || continue
        in_checkout "$cwd" || continue
        case "$(readlink "/proc/$pid/fd/0" 2>/dev/null)" in pipe:* | socket:*) ;; *) continue ;; esac
        if [ "$comm" = tail ]; then
            args=$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null) || continue
            [[ " $args" =~ [[:space:]](-[A-Za-z0-9]*[fF][A-Za-z0-9]*|--follow(=[a-z]+)?)[[:space:]] ]] && continue
        fi
        # utime and stime are fields 14 and 15; the fields after ") " start at
        # the state (field 3), so they are the 12th and 13th words.
        read -r stat <"/proc/$pid/stat" 2>/dev/null || continue
        # shellcheck disable=SC2086 # split on purpose
        set -- ${stat##*) }
        [ $((${12:-0} + ${13:-0})) -le "$idle_cpu_ticks" ] || continue
        again=$(agentic_bounded_call ps -o etimes= -p "$pid" 2>/dev/null) || continue
        again=${again//[[:space:]]/}
        case "$again" in '' | *[!0-9]*) continue ;; esac
        [ "$again" -gt "$idle_limit" ] && plausible "$again" || continue
        found+="pid=$pid elapsed=${elapsed}s comm=$comm cwd=$cwd idle-stdin-reader"$'\n'
    done < <(ps -eo pid=,etimes=,comm= | awk '$3=="cat"||$3=="tee"||$3=="head"||$3=="tail"')
    if [ -n "$found" ]; then
        echo "WATCHDOG $(date -u +%FT%TZ): long-running or idle processes in $root:"
        printf '%s' "$found"
        exit 3
    fi
    [ "$once" = 1 ] && exit 0
    sleep "$interval"
done

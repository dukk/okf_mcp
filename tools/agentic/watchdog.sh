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
# A third check covers an agent that stops without any process left to see
# (#850): `expect.sh start <worktree>` records that the coordinator waits for
# that agent's report. A pending expectation alerts when its worktree is idle:
# no test or gate process works there (a cargo, rustc, just or flock process,
# a gate.sh, or a test binary with its cwd in the worktree; a gate waiting for
# the shared gate lock counts as working) and the newest activity is older than
# AGENTIC_WATCHDOG_QUIET (default 1200 s). Activity is the newest of: any mtime
# under <worktree>/target/i*-logs, the worktree's HEAD reflog time and the
# expectation's start time (a time in the future counts as now). An expectation
# whose worktree is gone is dropped with a note. The alert names the worktree's
# basename, the label and the idle seconds. After an alert, `expect.sh done`
# or `expect.sh start` (which restarts the quiet clock) the expectation, or a
# restarted watchdog alerts again at once while it is still idle.
# It reports each process's pid, elapsed time, comm (the executable's name)
# and directory, never its command line: command lines can carry credentials.
# (Matching still reads the command line internally.)
# Every `ps` scan is bounded like the tools' gh and git calls (AGENTIC_CALL_TIMEOUT,
# default 60 s, #675). A scan that times out or fails skips that pass with a
# warning on stderr (never the scan's output) and the next pass tries again; it
# is not an alarm, and with --once it still exits 0.
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
quiet_limit=${AGENTIC_WATCHDOG_QUIET:-1200}
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

# Prints the newest mtime (whole seconds) of everything under $1's
# target/i*-logs directories, or nothing when there are none.
newest_log_time() {
    local dir newest=0 time
    for dir in "$1"/target/i*-logs; do
        [ -d "$dir" ] || continue
        time=$(agentic_bounded_call find "$dir" -printf '%T@\n' 2>/dev/null \
            | sort -n | tail -n 1)
        time=${time%%.*}
        case "$time" in '' | *[!0-9]*) continue ;; esac
        [ "$time" -gt "$newest" ] && newest=$time
    done
    [ "$newest" -gt 0 ] && printf '%s\n' "$newest"
    return 0
}

# scan_ps <variable> <ps args...>: runs `ps` bounded (lib.sh: agentic_bounded_call)
# and stores its output in <variable>. A `ps` that hangs or fails (for example on
# a /proc entry stuck in an uninterruptible read) is a skipped scan, not an
# alarm: it warns, without any output of the scan, and returns 1.
scan_ps() {
    local out status=0
    out=$(agentic_bounded_call ps "${@:2}") || status=$?
    if [ "$status" -ne 0 ]; then
        echo "watchdog: warning: ps scan failed or timed out (status $status); skipping it" >&2
        return 1
    fi
    printf -v "$1" '%s' "$out"
}

# Checks every pending expectation (expect.sh), appends alerts to $found.
check_expectations() {
    local dir file started label wt name now newest time idle busy cwd
    dir=$(agentic_expect_dir) || return 0
    [ -d "$dir" ] || return 0
    # Working processes: the cwd of each cargo, rustc, just or flock, of a
    # gate.sh (a bash, so by its argument) and of a test binary.
    local busy_cwds=() procs
    scan_ps procs -eo pid=,comm=,args= || return 0
    while read -r pid comm args; do
        case "$comm" in
            cargo | cargo-* | rustc | just | flock) ;;
            *) case "$args" in */gate.sh* | */debug/deps/* | */debug/build/*/out/*) ;; *) continue ;; esac ;;
        esac
        cwd=$(readlink "/proc/$pid/cwd" 2>/dev/null) && busy_cwds+=("$cwd")
    done < <(grep -v -E 'grep|watchdog\.sh' <<<"$procs")
    now=$(agentic_now)
    for file in "$dir"/*; do
        [ -f "$file" ] || continue
        wt=$(agentic_expect_field "$file" path)
        label=$(agentic_expect_field "$file" label)
        started=$(agentic_expect_field "$file" start)
        name=$(basename "$file")
        if [ ! -d "$wt" ]; then
            rm -f "$file"
            echo "watchdog: dropped the expectation for $name: its worktree is gone"
            continue
        fi
        case "$started" in '' | *[!0-9]*) started=$now ;; esac
        busy=0
        for cwd in "${busy_cwds[@]}"; do
            case "$cwd" in "$wt" | "$wt"/*) busy=1 ;; esac
        done
        [ "$busy" = 0 ] || continue
        newest=$started
        time=$(newest_log_time "$wt")
        [ -n "$time" ] && [ "$time" -gt "$newest" ] && newest=$time
        # The reflog event time (HEAD@{<epoch>}), not the commit's own date: a
        # checkout or reset to an old commit is fresh activity.
        time=$(agentic_git -C "$wt" reflog -1 --date=unix --format=%gd HEAD 2>/dev/null)
        time=${time#HEAD@\{}
        time=${time%\}}
        case "$time" in '' | *[!0-9]*) ;; *) [ "$time" -gt "$newest" ] && newest=$time ;; esac
        idle=$((now - newest))
        [ "$idle" -gt "$quiet_limit" ] || continue
        found+="agent at $name quiet for ${idle}s with a report pending: ${label:-no label}"$'\n'
    done
}

while :; do
    # Re-read every round: worktrees come and go during an epic. The call is
    # bounded (lib.sh: agentic_git), so a stuck git can't stall the watchdog.
    checkouts=("$root")
    while read -r path; do checkouts+=("$path"); done \
        < <(agentic_git -C "$root" worktree list --porcelain | sed -n 's/^worktree //p')
    found=""
    uptime=$(agentic_uptime)
    # Both scans are bounded. When one times out the whole pass is skipped
    # (with a warning from scan_ps, never an alarm); the next pass tries again.
    if ! { scan_ps all_procs -eo pid=,etimes=,args= && scan_ps readers -eo pid=,etimes=,comm=; }; then
        [ "$once" = 1 ] && exit 0
        sleep "$interval"
        continue
    fi
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
    done < <(grep -E '/debug/deps/|/debug/build/[^/ ]+/[^/ ]+/out/|cargo (test|clippy|build)|just (test|lint|test-ui)' <<<"$all_procs" \
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
    done < <(awk '$3=="cat"||$3=="tee"||$3=="head"||$3=="tail"' <<<"$readers")
    check_expectations
    if [ -n "$found" ]; then
        echo "WATCHDOG $(date -u +%FT%TZ): long-running or idle processes in $root:"
        printf '%s' "$found"
        exit 3
    fi
    [ "$once" = 1 ] && exit 0
    sleep "$interval"
done

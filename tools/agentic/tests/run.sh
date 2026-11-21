#!/usr/bin/env bash
# Shell tests for tools/agentic. Run with `just test-agentic`, or directly.
#
# A stub `gh` (stub/gh) sits first on PATH and answers from per-test fixture
# files, watch-review.sh runs on a fake clock (stub/fake-now, stub/fake-sleep),
# and every git repository is a throwaway one under a temporary directory, so
# nothing touches GitHub, the real checkout or the wall clock (the gate and
# watchdog tests wait about a second each).
#
# Exit status: 0 when every test passes, 1 otherwise.
set -u
tests_dir=$(cd "$(dirname "$0")" && pwd)
tools=$(dirname "$tests_dir")
work=$(mktemp -d)
work=$(cd "$work" && pwd -P)
trap 'rm -rf "$work"' EXIT

export PATH="$tests_dir/stub:$PATH"
export AGENTIC_REPO=owner/repo AGENTIC_RETRIGGER=/prrb
# The scripts' defaults are under test, so none of these come from the caller.
unset AGENTIC_REVIEWER AGENTIC_MAX_REVIEW_ROUNDS AGENTIC_NOW_CMD AGENTIC_SLEEP_CMD

bot='brooke-reviews[bot]'
head_a=aaaaaaa1111111111111111111111111111111111
head_b=bbbbbbb2222222222222222222222222222222222

git_quiet() {
    git -c user.name=test -c user.email=test@example.invalid \
        -c commit.gpgsign=false -c core.hooksPath=/dev/null "$@"
}

# A main checkout with an origin, and a linked worktree OUTSIDE the main
# checkout (a sibling directory), as `git worktree add ../wt` would make.
repo=$work/repo
git init -q -b main "$repo"
git_quiet -C "$repo" commit -q --allow-empty -m init
git clone -q --bare "$repo" "$work/origin.git"
git -C "$repo" remote add origin "$work/origin.git"
git -C "$repo" fetch -q origin
git -C "$repo" worktree add -q -b wt "$work/wt"
mkdir "$work/wt2" "$work/elsewhere"

# --- helpers (each test runs in its own subshell, so `exit 1` fails just it) ---

new_fixtures() {
    fx=$(mktemp -d "$work/fx.XXXXXX")
    mkdir "$fx/api"
    : >"$fx/calls.log"
    export GH_FIXTURES=$fx
}

# A user whose login shares the bot's prefix but is not the bot.
impostor='brooke-example'

# old_reviews <n>: n bot reviews, all of an earlier head than the PR's.
old_reviews() {
    jq -n --arg bot "$bot" --argjson n "$1" '[range($n) | {user: {login: $bot}, commit_id: "0ld"}]' \
        >"$fx/api/pulls_7_reviews.1.json"
}

# good_pr <head>: a PR whose every merge check passes for <head>.
good_pr() {
    printf '{"headRefOid":"%s","mergeStateStatus":"CLEAN","baseRefName":"main"}\n' "$1" \
        >"$fx/pr-view.json"
    printf '[{"user":{"login":"%s"},"commit_id":"%s"}]\n' "$bot" "$1" \
        >"$fx/api/pulls_7_reviews.1.json"
    printf '[{"id":1,"in_reply_to_id":null,"user":{"login":"%s"}},
             {"id":2,"in_reply_to_id":1,"user":{"login":"author"}}]\n' "$bot" \
        >"$fx/api/pulls_7_comments.1.json"
}

# A full first page of 30 bot reviews of an older commit; the current-head
# review then has to be on page two. The round cap is raised so that the 30
# old reviews alone can't satisfy the scripts.
old_reviews_page() {
    export AGENTIC_MAX_REVIEW_ROUNDS=100
    jq -n --arg bot "$bot" '[range(30) | {user: {login: $bot}, commit_id: "0ld"}]' \
        >"$fx/api/pulls_7_reviews.1.json"
    printf '[{"user":{"login":"%s"},"commit_id":"%s"}]\n' "$bot" "$head_a" \
        >"$fx/api/pulls_7_reviews.2.json"
}

die() {
    echo "  $*"
    printf '  output:\n%s\n' "${out:-}" | sed 's/^/    /'
    exit 1
}
want_rc() { [ "$rc" = "$1" ] || die "exit $rc, want $1"; }
want_out() { grep -qF -- "$1" <<<"$out" || die "output lacks '$1'"; }
want_no_out() { ! grep -qF -- "$1" <<<"$out" || die "output has '$1'"; }
want_not_merged() { [ ! -e "$fx/merged" ] || die "merged $(cat "$fx/merged")"; }

# The real-time timeout only stops a broken script from hanging the suite.
run_merge() {
    out=$(cd "$repo" && timeout 15 "$tools/merge-reviewed.sh" 7 2>&1)
    rc=$?
}

# block_gh: every gh call hangs from now on; the EXIT trap kills whatever the
# stub started, in case a script under test leaves it behind.
block_gh() {
    touch "$fx/block"
    trap '[ -s "$fx/blocked.pids" ] && xargs kill -KILL <"$fx/blocked.pids" 2>/dev/null' EXIT
}

# want_no_blocked_left: every process the blocking stub started is gone.
want_no_blocked_left() {
    [ -s "$fx/blocked.pids" ] || die "the stub was never called"
    local pid
    while read -r pid; do
        ! kill -0 "$pid" 2>/dev/null || die "stub process $pid outlived the script"
    done <"$fx/blocked.pids"
}

# The watcher on a fake clock starting at 1000 s. The real-time timeout only
# stops a broken watcher from hanging the suite; it is never reached when the
# watcher works.
run_watch() {
    echo 1000 >"$fx/clock"
    out=$(cd "$repo" && AGENTIC_NOW_CMD=fake-now AGENTIC_SLEEP_CMD=fake-sleep \
        timeout 15 "$tools/watch-review.sh" 7 2>&1)
    rc=$?
    clock=$(cat "$fx/clock")
}

# --- merge-reviewed.sh ---

test_merge_refuses_unreviewed_head() {
    new_fixtures; good_pr "$head_a"
    printf '[{"user":{"login":"%s"},"commit_id":"%s"}]\n' "$bot" "$head_b" \
        >"$fx/api/pulls_7_reviews.1.json"
    run_merge
    want_rc 1; want_out "REFUSE: no brooke-reviews[bot] review"; want_not_merged
}

test_merge_refuses_unreplied_thread() {
    new_fixtures; good_pr "$head_a"
    printf '[{"id":1,"in_reply_to_id":null,"user":{"login":"%s"}}]\n' "$bot" \
        >"$fx/api/pulls_7_comments.1.json"
    run_merge
    want_rc 1; want_out "without a reply: [1]"; want_not_merged
}

test_merge_refuses_unclean_state() {
    new_fixtures; good_pr "$head_a"
    printf '{"headRefOid":"%s","mergeStateStatus":"BEHIND","baseRefName":"main"}\n' "$head_a" >"$fx/pr-view.json"
    run_merge
    want_rc 1; want_out "mergeStateStatus=BEHIND"; want_not_merged
}

test_merge_refuses_on_api_error() {
    new_fixtures; good_pr "$head_a"
    touch "$fx/api/pulls_7_reviews.fail"
    run_merge
    want_rc 1; want_out "REFUSE: could not read the reviews"; want_not_merged
}

test_merge_refuses_on_pr_view_error() {
    new_fixtures; good_pr "$head_a"; touch "$fx/pr-view.fail"
    run_merge
    want_rc 1; want_out "REFUSE: could not read PR 7"; want_not_merged
}

test_merge_refuses_on_comments_api_error() {
    new_fixtures; good_pr "$head_a"; touch "$fx/api/pulls_7_comments.fail"
    run_merge
    want_rc 1; want_out "REFUSE: could not read the inline comments"; want_not_merged
}

test_merge_ignores_prefix_sharing_reviewer() {
    new_fixtures; good_pr "$head_a"
    # Three reviews of the current head by someone who is not the bot: they
    # must neither count as the head's review nor use up rounds.
    jq -n --arg who "$impostor" --arg head "$head_a" \
        '[range(3) | {user: {login: $who}, commit_id: $head}]' >"$fx/api/pulls_7_reviews.1.json"
    run_merge
    want_rc 1; want_out "(0 of 3 rounds used)"; want_not_merged
}

test_merge_ignores_prefix_sharing_thread() {
    new_fixtures; good_pr "$head_a"
    printf '[{"id":5,"in_reply_to_id":null,"user":{"login":"%s"}}]\n' "$impostor" \
        >"$fx/api/pulls_7_comments.1.json"
    run_merge
    want_rc 0; want_out "MERGED PR 7"
}

test_merge_matches_bot_login_without_suffix() {
    new_fixtures; good_pr "$head_a"
    out=$(cd "$repo" && AGENTIC_REVIEWER=brooke-reviews "$tools/merge-reviewed.sh" 7 2>&1)
    rc=$?
    want_rc 0; want_out "MERGED PR 7"
}

test_merge_refuses_on_hung_call() {
    new_fixtures; good_pr "$head_a"; block_gh
    started=$SECONDS
    AGENTIC_CALL_TIMEOUT=1 run_merge
    want_rc 1; want_out "REFUSE: could not read PR 7"; want_not_merged
    [ $((SECONDS - started)) -le 4 ] || die "took $((SECONDS - started)) s"
    want_no_blocked_left
}

test_merge_refuses_head_changed_before_merge() {
    new_fixtures; good_pr "$head_a"
    echo "$head_b" >"$fx/merge-head"
    run_merge
    want_rc 1; want_out "REFUSE: the merge of aaaaaaa failed"; want_not_merged
}

test_merge_finds_review_on_page_two() {
    new_fixtures; good_pr "$head_a"; old_reviews_page
    run_merge
    want_rc 0; want_out "MERGED PR 7"; want_no_out "round cap"
}

test_merge_below_cap_requires_current_head_review() {
    new_fixtures; good_pr "$head_a"; old_reviews 2
    run_merge
    want_rc 1; want_out "REFUSE: no brooke-reviews[bot] review on head aaaaaaa (2 of 3 rounds used)"
    want_not_merged
}

test_merge_at_cap_merges_newer_head() {
    new_fixtures; good_pr "$head_a"; old_reviews 3
    run_merge
    want_rc 0; want_out "under the review round cap (3 of 3 rounds"; want_out "MERGED PR 7"
    grep -qF -- "--match-head-commit $head_a" "$fx/calls.log" || die "merge not pinned"
}

test_merge_at_cap_refuses_unreplied_thread() {
    new_fixtures; good_pr "$head_a"; old_reviews 3
    printf '[{"id":1,"in_reply_to_id":null,"user":{"login":"%s"}}]\n' "$bot" \
        >"$fx/api/pulls_7_comments.1.json"
    run_merge
    want_rc 1; want_out "without a reply: [1]"; want_not_merged
}

test_merge_at_cap_refuses_unclean_state() {
    new_fixtures; good_pr "$head_a"; old_reviews 3
    printf '{"headRefOid":"%s","mergeStateStatus":"BLOCKED","baseRefName":"main"}\n' "$head_a" >"$fx/pr-view.json"
    run_merge
    want_rc 1; want_out "mergeStateStatus=BLOCKED"; want_not_merged
}

test_merge_merges_when_all_checks_pass() {
    new_fixtures; good_pr "$head_a"
    run_merge
    want_rc 0; want_out "MERGED PR 7 -> main"
    [ "$(cat "$fx/merged")" = "$head_a" ] || die "merged the wrong head"
    grep -qF -- "pr merge 7 -R owner/repo --merge --match-head-commit $head_a" "$fx/calls.log" \
        || die "merge not pinned to the head"
}

# base_pr <head> <base> [<merge commit>]: good_pr, but with a PR base other than main.
base_pr() {
    good_pr "$1"
    jq -n --arg h "$1" --arg b "$2" --arg m "${3:-}" \
        '{headRefOid: $h, mergeStateStatus: "CLEAN", baseRefName: $b}
         + (if $m == "" then {} else {mergeCommit: {oid: $m}} end)' >"$fx/pr-view.json"
}

test_merge_reports_the_base_and_merge_commit() {
    new_fixtures; base_pr "$head_a" milestone/9-x ccccccc3333333333333333333333333333333333
    run_merge
    want_rc 0; want_out "MERGED PR 7 -> milestone/9-x ccccccc"; want_no_out "-> main"
}

test_merge_falls_back_to_the_base_tip() {
    new_fixtures; base_pr "$head_a" milestone/9-x
    # Put the base on origin one commit ahead of main, so its tip is not main's.
    git_quiet -C "$repo" branch -f ms main
    git_quiet -C "$repo" worktree add -q --detach "$work/ms" ms
    git_quiet -C "$work/ms" commit -q --allow-empty -m ms
    git_quiet -C "$work/ms" push -q origin HEAD:refs/heads/milestone/9-x
    tip=$(git -C "$work/ms" rev-parse --short HEAD)
    git -C "$repo" worktree remove --force "$work/ms"
    git -C "$repo" branch -D ms -q
    run_merge
    want_rc 0; want_out "MERGED PR 7 -> milestone/9-x $tip"
}

test_merge_refuses_without_a_base_branch() {
    new_fixtures; good_pr "$head_a"
    # A PR with no baseRefName at all, then one whose baseRefName is null.
    printf '{"headRefOid":"%s","mergeStateStatus":"CLEAN"}\n' "$head_a" >"$fx/pr-view.json"
    run_merge
    want_rc 1; want_out "REFUSE: could not read the base branch of PR 7"; want_not_merged
    printf '{"headRefOid":"%s","mergeStateStatus":"CLEAN","baseRefName":null}\n' "$head_a" \
        >"$fx/pr-view.json"
    run_merge
    want_rc 1; want_out "REFUSE: could not read the base branch of PR 7"; want_not_merged
}

test_merge_keeps_the_sha_when_origin_is_unavailable() {
    new_fixtures; base_pr "$head_a" milestone/9-x ccccccc3333333333333333333333333333333333
    # The merge has happened; a fetch that fails must not hide it. Put origin
    # back however the test ends (the repository is shared by every test).
    trap 'git -C "$repo" remote set-url origin "$work/origin.git"' EXIT
    git -C "$repo" remote set-url origin "$work/no-such-origin.git"
    run_merge
    want_rc 0; want_out "MERGED PR 7 -> milestone/9-x ccccccc"
    want_out "warning: could not fetch origin afterwards"
    [ "$(cat "$fx/merged")" = "$head_a" ] || die "merged the wrong head"
}

test_merge_warns_when_no_merge_commit_can_be_found() {
    # No mergeCommit from gh, and origin has no such base branch.
    new_fixtures; base_pr "$head_a" milestone/absent
    run_merge
    want_rc 0
    want_out "MERGED PR 7 -> milestone/absent; warning: could not find the merge commit"
}

# --- watch-review.sh ---

test_watch_succeeds_on_current_head_review() {
    new_fixtures; good_pr "$head_a"
    run_watch
    want_rc 0; want_out "reviewed PR 7 at aaaaaaa"
}

test_watch_finds_review_on_page_two() {
    new_fixtures; good_pr "$head_a"; old_reviews_page
    run_watch
    want_rc 0; want_out "reviewed PR 7"
}

test_watch_at_cap_exits_4_without_retrigger() {
    new_fixtures; good_pr "$head_a"; old_reviews 3
    run_watch
    want_rc 4; want_out "review round limit reached (3)"
    [ "$clock" = 1000 ] || die "waited until $clock instead of exiting at once"
    ! grep -q '^pr comment' "$fx/calls.log" || die "re-triggered at the cap"
}

test_watch_gives_up_when_pr_view_keeps_failing() {
    new_fixtures; good_pr "$head_a"; touch "$fx/pr-view.fail"
    printf '[{"user":{"login":"%s"},"body":"skipped: out of date with main"}]\n' "$bot" \
        >"$fx/api/issues_7_comments.1.json"
    run_watch
    want_rc 2; want_out "TIMEOUT"; want_out "out of date with main"
    [ "$clock" -ge 2800 ] && [ "$clock" -le 2830 ] || die "gave up at $clock, want 2800..2830"
}

test_watch_retrigger_keeps_overall_deadline() {
    new_fixtures; good_pr "$head_a"
    echo '[]' >"$fx/api/pulls_7_reviews.1.json"
    run_watch
    want_rc 2; want_out "posted /prrb on PR 7"; want_out "TIMEOUT"
    [ "$(grep -c '^pr comment 7 .*--body /prrb' "$fx/calls.log")" = 1 ] || die "re-triggered more than once"
    [ "$clock" -ge 2800 ] && [ "$clock" -le 2830 ] || die "gave up at $clock, want 2800..2830"
}

# --- watchdog.sh ---

test_watch_reviews_api_error_never_retriggers() {
    new_fixtures; good_pr "$head_a"; touch "$fx/api/pulls_7_reviews.fail"
    run_watch
    want_rc 2; want_out "TIMEOUT"; want_out "could not read the reviews"
    ! grep -q '^pr comment' "$fx/calls.log" || die "re-triggered with the round count unknown"
    [ "$clock" -ge 2800 ] && [ "$clock" -le 2830 ] || die "gave up at $clock, want 2800..2830"
}

test_watch_reactions_api_error_never_retriggers() {
    new_fixtures; good_pr "$head_a"
    echo '[]' >"$fx/api/pulls_7_reviews.1.json"
    touch "$fx/api/issues_7_reactions.fail"
    run_watch
    want_rc 2
    ! grep -q '^pr comment' "$fx/calls.log" || die "re-triggered with the reactions unknown"
}

test_watch_ignores_prefix_sharing_reaction() {
    new_fixtures; good_pr "$head_a"
    echo '[]' >"$fx/api/pulls_7_reviews.1.json"
    printf '[{"user":{"login":"%s"},"content":"eyes"}]\n' "$impostor" \
        >"$fx/api/issues_7_reactions.1.json"
    run_watch
    # The bot hasn't reacted, so the re-trigger is due after ~90 s (the first
    # poll past 90 s is at 1120), not the ~6 min it waits after a bot reaction.
    [ "$(cat "$fx/comment-times" 2>/dev/null)" = 1120 ] \
        || die "re-triggered at $(cat "$fx/comment-times" 2>/dev/null), want 1120"
}

test_watch_bot_reaction_delays_retrigger() {
    new_fixtures; good_pr "$head_a"
    echo '[]' >"$fx/api/pulls_7_reviews.1.json"
    printf '[{"user":{"login":"%s"},"content":"eyes"}]\n' "$bot" \
        >"$fx/api/issues_7_reactions.1.json"
    run_watch
    [ "$(cat "$fx/comment-times" 2>/dev/null)" = 1390 ] \
        || die "re-triggered at $(cat "$fx/comment-times" 2>/dev/null), want 1390"
}

test_watch_never_retriggers_while_behind_main() {
    new_fixtures; good_pr "$head_a"
    echo '[]' >"$fx/api/pulls_7_reviews.1.json"
    printf '{"headRefOid":"%s","mergeStateStatus":"BEHIND"}\n' "$head_a" >"$fx/pr-view.json"
    run_watch
    want_rc 2; want_out "behind main: merge origin/main and push"
    ! grep -q '^pr comment' "$fx/calls.log" || die "re-triggered while behind main"
}

test_watch_stalled_gh_still_ends_at_deadline() {
    new_fixtures; good_pr "$head_a"; block_gh
    # Real clock with a 2 s deadline: a stalled gh must not hold the watcher
    # past it, and the diagnostics get only their own few seconds.
    started=$SECONDS
    out=$(cd "$repo" && AGENTIC_WATCH_TIMEOUT=2 timeout 15 "$tools/watch-review.sh" 7 2>&1)
    rc=$?
    want_rc 2; want_out "TIMEOUT"
    [ $((SECONDS - started)) -le 8 ] || die "took $((SECONDS - started)) s, want about 5"
    want_no_blocked_left
}

test_watchdog_covers_worktrees_outside_main_checkout() {
    new_fixtures
    # Not local: the EXIT trap runs after the function returns. The fakes'
    # output goes to /dev/null so they don't hold the runner's pipe open.
    fake_pids=()
    trap 'kill "${fake_pids[@]}" 2>/dev/null' EXIT
    local inside outside_sibling outside_prefix
    # Fake test binaries: argv[0] names a test binary, the process is a sleep.
    (cd "$work/wt" && exec -a "$work/wt/target/debug/deps/fake_test" sleep 30) >/dev/null 2>&1 &
    inside=$!; fake_pids+=("$inside")
    (cd "$work/wt2" && exec -a "$work/wt2/target/debug/deps/fake_test" sleep 30) >/dev/null 2>&1 &
    outside_prefix=$!; fake_pids+=("$outside_prefix")
    (cd "$work/elsewhere" && exec -a "$work/elsewhere/target/debug/deps/fake_test" sleep 30) >/dev/null 2>&1 &
    outside_sibling=$!; fake_pids+=("$outside_sibling")
    sleep 1.2 # so every fake has run longer than the 0 s limit
    out=$(cd "$work/wt" && AGENTIC_WATCHDOG_TEST=0 AGENTIC_WATCHDOG_GATE=0 \
        timeout 15 "$tools/watchdog.sh" --once 2>&1)
    rc=$?
    want_rc 3; want_out "pid=$inside "
    grep -qE "pid=($outside_prefix|$outside_sibling) " <<<"$out" && die "reported a process outside"
    kill "$inside"; wait "$inside" 2>/dev/null
    out=$(cd "$work/wt" && AGENTIC_WATCHDOG_TEST=0 AGENTIC_WATCHDOG_GATE=0 \
        timeout 15 "$tools/watchdog.sh" --once 2>&1)
    rc=$?
    want_rc 0
}

# watchdog_with_ps <first etimes> <re-read etimes|gone> [uptime]: runs the
# watchdog once, with a stub `ps` that lists one test process in the checkout
# (a real sleep, so /proc/<pid>/cwd resolves) with the first etimes, then
# answers a re-read with the second (or fails when "gone"). Limits are 1200 s.
watchdog_with_ps() {
    new_fixtures
    fake_pids=()
    trap 'kill "${fake_pids[@]}" 2>/dev/null' EXIT
    (cd "$work/wt" && exec -a "$work/wt/target/debug/deps/fake_test" sleep 30) >/dev/null 2>&1 &
    ps_pid=$!; fake_pids+=("$ps_pid")
    sleep 0.3
    mkdir -p "$fx/bin"
    printf '%s\n' "${3:-693000.50} 1.00" >"$fx/uptime"
    cat >"$fx/bin/ps" <<STUB
#!/usr/bin/env bash
case "\$*" in
    *"-p "*) [ "$2" = gone ] && exit 1; echo "  $2" ;;
    *) echo "$ps_pid $1 $work/wt/target/debug/deps/fake_test" ;;
esac
STUB
    chmod +x "$fx/bin/ps"
    out=$(cd "$work/wt" && PATH="$fx/bin:$PATH" AGENTIC_UPTIME_FILE="$fx/uptime" \
        timeout 15 "$tools/watchdog.sh" --once 2>&1)
    rc=$?
}

test_watchdog_ignores_a_wrapped_etimes_reading() {
    # The 2026-11-18 sighting: 2^32 - 4123168608 = 171798688.
    watchdog_with_ps 4123168608 5001
    want_rc 0
    ! grep -q WATCHDOG <<<"$out" || die "alarmed on a reading above the uptime"
}

test_watchdog_ignores_a_process_gone_on_reread() {
    watchdog_with_ps 5000 gone
    want_rc 0
    ! grep -q WATCHDOG <<<"$out" || die "alarmed on a process that exited"
}

test_watchdog_ignores_an_implausible_reread() {
    watchdog_with_ps 5000 4123168608
    want_rc 0
}

test_watchdog_alarms_on_a_genuinely_long_process() {
    watchdog_with_ps 5000 5001
    want_rc 3; want_out "pid=$ps_pid elapsed=5000s comm=sleep cwd=$work/wt"
    # Alerts carry no command line: argv[0] here is a path to the fake binary.
    ! grep -q "fake_test" <<<"$out" || die "the alert includes command-line text"
}

test_watchdog_sees_a_test_binary_in_the_build_dir_layout() {
    # Issue #738: cargo 1.100 runs test binaries from
    # <build-dir>/debug/build/<package>/<hash>/out/, with no /debug/deps/ in
    # the path, and the watchdog missed a lib test binary hung for 50 min.
    # The gate limit is out of reach, so only the test-binary limit can fire.
    new_fixtures
    fake_pids=()
    trap 'kill "${fake_pids[@]}" 2>/dev/null' EXIT
    local binary=$work/build-dir/debug/build/okf_mcp/0e3f1e14f080a04c/out/okf_mcp-0e3f1e14f080a04c
    (cd "$work/wt" && exec -a "$binary" sleep 30) >/dev/null 2>&1 &
    local hung=$!
    fake_pids+=("$hung")
    sleep 1.2 # so the fake has run longer than the 0 s limit
    out=$(cd "$work/wt" && AGENTIC_WATCHDOG_TEST=0 AGENTIC_WATCHDOG_GATE=999999 \
        timeout 15 "$tools/watchdog.sh" --once 2>&1)
    rc=$?
    want_rc 3; want_out "pid=$hung "
    ! grep -q "okf_mcp-0e3f1e14f080a04c" <<<"$out" || die "the alert includes command-line text"
    kill "$hung"; wait "$hung" 2>/dev/null || true
}

# --- idle stdin readers (issue #780) ---

# idle_reader <comm> [args...]: starts the real <comm> in $work/wt with a pipe
# on stdin that nothing ever writes to or closes (a 20 s sleep holds it open,
# so the test is bounded), and sets reader_pid. Output goes to /dev/null so the
# fake doesn't hold the runner's pipe open.
idle_reader() {
    (cd "$work/wt" && exec "$@" < <(sleep 20)) >/dev/null 2>&1 &
    reader_pid=$!; fake_pids+=("$reader_pid")
}

# idle_watchdog [env...]: one watchdog pass from $work/wt with the test and gate
# limits out of reach, so only the idle check can fire.
idle_watchdog() {
    out=$(cd "$work/wt" && env AGENTIC_WATCHDOG_TEST=999999 AGENTIC_WATCHDOG_GATE=999999 "$@" \
        timeout 15 "$tools/watchdog.sh" --once 2>&1)
    rc=$?
}

test_watchdog_flags_an_idle_cat_on_a_pipe_without_argument_text() {
    new_fixtures; fake_pids=(); trap 'kill "${fake_pids[@]}" 2>/dev/null' EXIT
    # tee reads stdin like cat and takes a distinctive argument to look for.
    idle_reader tee "$work/wt/distinct-secret-arg.out"
    local tee_pid=$reader_pid
    idle_reader cat
    sleep 1.2 # so both have run longer than the 0 s limit
    idle_watchdog AGENTIC_WATCHDOG_IDLE=0
    want_rc 3; want_out "pid=$reader_pid "; want_out "comm=cat cwd=$work/wt"
    want_out "pid=$tee_pid "; want_out "comm=tee "
    ! grep -q "distinct-secret-arg" <<<"$out" || die "the alert includes argument text"
    ! grep -q "cmd=" <<<"$out" || die "the alert has a cmd field"
}

test_watchdog_ignores_a_sleep_and_a_cat_on_a_file() {
    new_fixtures; fake_pids=()
    # A stopped process ignores SIGTERM until it is continued, so the cleanup
    # continues every fake first, whether the test passes or dies.
    trap 'kill -CONT "${fake_pids[@]}" 2>/dev/null; kill "${fake_pids[@]}" 2>/dev/null' EXIT
    (cd "$work/wt" && exec sleep 20) >/dev/null 2>&1 &
    fake_pids+=("$!")
    # A cat whose stdin is a regular file: fd 0 is not a pipe or socket. It must
    # still be alive during the scan, or the test proves nothing (a cat on an
    # empty file exits at once). A 1 MiB file and a stdout pipe nobody reads
    # keep it blocked in a write, and a SIGSTOP holds it in state T.
    head -c 1048576 /dev/zero >"$work/regular-input"
    (cd "$work/wt" && exec cat < "$work/regular-input" > >(sleep 20)) >/dev/null 2>&1 &
    local file_cat=$!
    fake_pids+=("$file_cat")
    # Stop it only once it has exec'd cat: until then it is the subshell.
    local tries=0 state=
    while [ "$(cat "/proc/$file_cat/comm" 2>/dev/null)" != cat ] && [ "$tries" -lt 50 ]; do
        tries=$((tries + 1)); sleep 0.1
    done
    kill -STOP "$file_cat"
    tries=0
    while [ "$tries" -lt 50 ]; do
        state=$(awk '/^State:/ {print $2}' "/proc/$file_cat/status" 2>/dev/null)
        [ "$state" = T ] && break
        tries=$((tries + 1)); sleep 0.1
    done
    [ "$state" = T ] || die "the cat on a file is not alive and stopped (state: ${state:-gone})"
    [ "$(readlink "/proc/$file_cat/fd/0")" = "$work/regular-input" ] || die "fd 0 is not the regular file"
    # tail -f is a follower, not a stuck reader.
    idle_reader tail -f /dev/stdin
    sleep 1.2
    idle_watchdog AGENTIC_WATCHDOG_IDLE=0
    want_rc 0
    kill -0 "$file_cat" 2>/dev/null || die "the cat on a file exited before the scan"
    kill -CONT "$file_cat"; kill "$file_cat"; wait "$file_cat" 2>/dev/null || true
    [ ! -d "/proc/$file_cat" ] || die "the cat on a file outlived the test"
}

test_watchdog_ignores_an_idle_reader_outside_a_checkout() {
    new_fixtures; fake_pids=(); trap 'kill "${fake_pids[@]}" 2>/dev/null' EXIT
    # Two identical readers on pipes, one inside a checkout and one outside
    # every checkout: only the directory differs, so only the checkout filter
    # can tell them apart.
    idle_reader cat
    local inside=$reader_pid outside
    (cd "$work/elsewhere" && exec cat < <(sleep 20)) >/dev/null 2>&1 &
    outside=$!; fake_pids+=("$outside")
    sleep 1.2
    [ "$(readlink "/proc/$outside/fd/0" | cut -d: -f1)" = pipe ] || die "the outside reader is not on a pipe"
    idle_watchdog AGENTIC_WATCHDOG_IDLE=0
    want_rc 3; want_out "pid=$inside "
    ! grep -q "pid=$outside " <<<"$out" || die "reported a reader outside every checkout"
}

test_watchdog_ignores_an_idle_reader_under_the_limit() {
    new_fixtures; fake_pids=(); trap 'kill "${fake_pids[@]}" 2>/dev/null' EXIT
    idle_reader cat
    sleep 1.2 # older than 0 s, far under the 3600 s limit
    idle_watchdog AGENTIC_WATCHDOG_IDLE=3600
    want_rc 0
}

test_watchdog_ignores_a_reader_that_uses_cpu() {
    new_fixtures; fake_pids=(); trap 'kill "${fake_pids[@]}" 2>/dev/null' EXIT
    # cat copying from a pipe that never stops producing is busy, not stuck.
    (cd "$work/wt" && exec cat < <(timeout 4 yes)) >/dev/null 2>&1 &
    fake_pids+=("$!")
    sleep 3
    # A zero-tick CPU allowance, so any CPU at all counts as busy.
    idle_watchdog AGENTIC_WATCHDOG_IDLE=0 AGENTIC_WATCHDOG_IDLE_CPU=0
    want_rc 0
    sleep 2 # let the 4 s producer end so no cat outlives the test
}

# idle_watchdog_with_ps <etimes>: a stub `ps` lists one real idle cat in the
# checkout with the given etimes (and answers the re-read with it), and the
# idle limit is left unset, so the default applies. The caller sets up the
# fixtures and the cleanup trap first, so the helper can run twice in one test.
idle_watchdog_with_ps() {
    idle_reader cat
    sleep 0.3
    mkdir -p "$fx/bin"
    printf '%s\n' "693000.50 1.00" >"$fx/uptime"
    cat >"$fx/bin/ps" <<STUB
#!/usr/bin/env bash
case "\$*" in
    *"-p "*) echo "  $1" ;;
    *args=*) ;;
    *) echo "$reader_pid $1 cat" ;;
esac
STUB
    chmod +x "$fx/bin/ps"
    idle_watchdog PATH="$fx/bin:$PATH" AGENTIC_UPTIME_FILE="$fx/uptime"
}

test_watchdog_default_idle_limit_is_600_seconds() {
    new_fixtures; fake_pids=(); trap 'kill "${fake_pids[@]}" 2>/dev/null' EXIT
    unset AGENTIC_WATCHDOG_IDLE
    idle_watchdog_with_ps 601
    want_rc 3; want_out "pid=$reader_pid elapsed=601s comm=cat"
    idle_watchdog_with_ps 599
    want_rc 0
}

test_watchdog_ignores_an_idle_reader_with_a_wrapped_etimes() {
    new_fixtures; fake_pids=(); trap 'kill "${fake_pids[@]}" 2>/dev/null' EXIT
    idle_watchdog_with_ps 4123168608
    want_rc 0
}

# --- gate.sh ---

test_gate_timeout_returns_124() {
    new_fixtures; fake_just 'exec sleep 30'
    out=$(cd "$repo" && PATH="$fx/bin:$PATH" AGENTIC_TIMEOUT_FMT=1s \
        "$tools/gate.sh" fmt 2>&1)
    rc=$?
    want_rc 124; want_out "Treat this as a HANG"
}

# fake_just <body>: puts a `just` running <body> first on PATH for this test.
fake_just() {
    mkdir -p "$fx/bin"
    printf '#!/usr/bin/env bash\n%s\n' "$1" >"$fx/bin/just"
    chmod +x "$fx/bin/just"
}

test_gate_returns_the_gate_exit_status() {
    new_fixtures; fake_just 'exit 3'
    out=$(cd "$repo" && PATH="$fx/bin:$PATH" "$tools/gate.sh" lint 2>&1)
    rc=$?
    want_rc 3; want_no_out "HANG"
}

# leaky_gate <then>: a fake gate that starts a child which ignores TERM and
# runs in the gate's process group, then runs <then> (wait, exit 0, kill itself...).
# The child's output goes to /dev/null so it can't hold the test's pipe open.
leaky_gate() {
    fake_just "bash -c 'trap \"\" TERM; exec sleep 30' >/dev/null 2>&1 &
echo \$! >\"\$GH_FIXTURES/child.pid\"
$1"
    trap '[ -s "$fx/child.pid" ] && kill -KILL "$(cat "$fx/child.pid")" 2>/dev/null' EXIT
}

# run_leaky_gate: runs the lint gate with a 1 s limit and 1 s grace.
run_leaky_gate() {
    started=$SECONDS
    (cd "$repo" && PATH="$fx/bin:$PATH" AGENTIC_TIMEOUT_LINT=1s AGENTIC_GATE_KILL_AFTER=1 \
        "$tools/gate.sh" lint) >"$fx/out" 2>&1
    rc=$?
    out=$(cat "$fx/out")
}

# want_nothing_left: gate.sh returned quickly, its TERM-ignoring child is gone
# and the lock is free.
want_nothing_left() {
    [ $((SECONDS - started)) -le 6 ] || die "took $((SECONDS - started)) s"
    [ -s "$fx/child.pid" ] || die "the fake gate didn't start its child"
    ! kill -0 "$(cat "$fx/child.pid")" 2>/dev/null || die "the TERM-ignoring child outlived the gate"
    flock -n "$repo/.git/agentic/gates.lock" true || die "gates.lock is still held"
}

test_gate_timeout_stops_term_ignoring_child_and_frees_lock() {
    new_fixtures; leaky_gate 'wait'
    run_leaky_gate
    want_rc 124; want_out "Treat this as a HANG"; want_nothing_left
}

test_gate_killed_leader_stops_child_and_frees_lock() {
    new_fixtures; leaky_gate 'kill -KILL $$'
    run_leaky_gate
    want_rc 124; want_out "Treat this as a HANG"; want_nothing_left
}

test_gate_passing_leader_stops_leftover_child_and_frees_lock() {
    new_fixtures; leaky_gate 'exit 0'
    run_leaky_gate
    want_rc 0; want_out "leaving 1 process(es) behind"; want_no_out "HANG"; want_nothing_left
}

# A daemon the gate starts (sccache's server, say) leaves the gate's process
# group with setsid, so the group cleanup never stops it. It must not take the
# lock with it: flock -o keeps the lock's fd out of the gate's children.
test_gate_daemon_that_escapes_the_group_does_not_keep_the_lock() {
    new_fixtures
    # shellcheck disable=SC2016 # $! and $GH_FIXTURES are expanded by the fake just
    fake_just 'setsid bash -c "exec sleep 30" </dev/null >/dev/null 2>&1 &
echo $! >"$GH_FIXTURES/daemon.pid"
exit 0'
    trap '[ -s "$fx/daemon.pid" ] && kill -KILL "$(cat "$fx/daemon.pid")" 2>/dev/null' EXIT
    out=$(cd "$repo" && PATH="$fx/bin:$PATH" "$tools/gate.sh" lint 2>&1)
    rc=$?
    want_rc 0
    [ -s "$fx/daemon.pid" ] || die "the fake gate didn't start its daemon"
    kill -0 "$(cat "$fx/daemon.pid")" 2>/dev/null || die "the daemon didn't outlive the gate"
    flock -n "$repo/.git/agentic/gates.lock" true || die "the daemon still holds gates.lock"
}

# The lock outlives the gate's own command: while gate.sh is still stopping a
# TERM-ignoring leftover, no other gate may take it.
test_gate_holds_the_lock_until_its_cleanup_ends() {
    new_fixtures; leaky_gate 'exit 0'
    (cd "$repo" && PATH="$fx/bin:$PATH" AGENTIC_GATE_KILL_AFTER=3 \
        "$tools/gate.sh" lint) >"$fx/out" 2>&1 &
    gate_pid=$!
    for ((tick = 0; tick < 50; tick++)); do
        [ -s "$fx/child.pid" ] && break
        sleep 0.1
    done
    [ -s "$fx/child.pid" ] || die "the fake gate didn't start its child"
    sleep 1 # the leader has exited; cleanup is waiting out its 3 s grace
    kill -0 "$(cat "$fx/child.pid")" 2>/dev/null || die "the leftover is already gone"
    ! flock -n "$repo/.git/agentic/gates.lock" true || die "the lock is free during cleanup"
    wait "$gate_pid"
    rc=$?
    out=$(cat "$fx/out")
    want_rc 0; want_out "leaving 1 process(es) behind"
    ! kill -0 "$(cat "$fx/child.pid")" 2>/dev/null || die "the leftover outlived the gate"
    flock -n "$repo/.git/agentic/gates.lock" true || die "gates.lock is still held"
}

test_gate_lock_wait_is_bounded() {
    new_fixtures; fake_just 'exit 0'
    mkdir -p "$repo/.git/agentic"
    # One process takes the lock and becomes the sleep, so killing $holder
    # releases the lock.
    # shellcheck disable=SC2016 # $1 is expanded by the inner bash
    bash -c 'exec 9>"$1"; flock 9; exec sleep 30' _ "$repo/.git/agentic/gates.lock" \
        >/dev/null 2>&1 &
    lock_holder=$! # not local: the EXIT trap runs after the function returns
    trap 'kill "$lock_holder" 2>/dev/null' EXIT
    sleep 0.2
    out=$(cd "$repo" && PATH="$fx/bin:$PATH" AGENTIC_TIMEOUT_LINT=1s "$tools/gate.sh" lint 2>&1)
    rc=$?
    kill "$lock_holder"; wait "$lock_holder" 2>/dev/null
    want_rc 124; want_out "Treat this as a HANG"
}

test_gate_dylint_runs_the_just_recipe() {
    new_fixtures; fake_just 'printf "%s\n" "$*" >"$GH_FIXTURES/just_args"; exit 0'
    out=$(cd "$repo" && PATH="$fx/bin:$PATH" "$tools/gate.sh" dylint 2>&1)
    rc=$?
    want_rc 0
    [ "$(cat "$fx/just_args")" = dylint ] \
        || die "just got '$(cat "$fx/just_args")', want 'dylint'"
}

test_gate_dylint_timeout_returns_124() {
    new_fixtures; fake_just 'exec sleep 30'
    out=$(cd "$repo" && PATH="$fx/bin:$PATH" AGENTIC_TIMEOUT_DYLINT=1s \
        "$tools/gate.sh" dylint 2>&1)
    rc=$?
    want_rc 124; want_out "Treat this as a HANG"
}

test_gate_narrow_builds_offline() {
    new_fixtures
    mkdir -p "$fx/bin"
    # A fake cargo that records the SQLX_OFFLINE it was given.
    cat >"$fx/bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "${SQLX_OFFLINE-unset}" >"$GH_FIXTURES/sqlx_offline"
EOF
    chmod +x "$fx/bin/cargo"
    out=$(cd "$repo" && env -u SQLX_OFFLINE DATABASE_URL=postgres://db.example.invalid/x \
        PATH="$fx/bin:$PATH" "$tools/gate.sh" narrow --lib 2>&1)
    rc=$?
    want_rc 0
    [ "$(cat "$fx/sqlx_offline")" = true ] \
        || die "cargo got SQLX_OFFLINE=$(cat "$fx/sqlx_offline"), want true"
}

# --- expect.sh and the watchdog's quiet-agent check (#850) ---

# expect_wt <name>: a fresh linked worktree $work/<name> and the real time in
# $t0. The fake clock starts at $t0, so the worktree's HEAD reflog is not newer
# than the expectation.
expect_wt() {
    rm -rf "$repo/.git/agentic-expect" # every test starts with an empty ledger
    git -C "$repo" worktree add -q -b "$1" "$work/$1"
    t0=$(date +%s)
    echo "$t0" >"$fx/clock"
}

# expect_watchdog <seconds after t0>: one watchdog pass on the fake clock, with
# the process limits out of reach so only the quiet-agent check can fire.
expect_watchdog() {
    echo $((t0 + $1)) >"$fx/clock"
    out=$(cd "$repo" && env AGENTIC_NOW_CMD=fake-now AGENTIC_WATCHDOG_TEST=999999 \
        AGENTIC_WATCHDOG_GATE=999999 timeout 15 "$tools/watchdog.sh" --once 2>&1)
    rc=$?
}

expect_start() {
    (cd "$repo" && AGENTIC_NOW_CMD=fake-now "$tools/expect.sh" start "$@") >/dev/null
}

test_expect_start_list_done() {
    new_fixtures; expect_wt ex-list
    expect_start "$work/ex-list" "task 5"
    echo $((t0 + 90)) >"$fx/clock"
    out=$(cd "$repo" && AGENTIC_NOW_CMD=fake-now "$tools/expect.sh" list 2>&1); rc=$?
    want_rc 0; want_out "ex-list waiting 90s: task 5"
    out=$(cd "$repo" && "$tools/expect.sh" done "$work/ex-list" 2>&1); rc=$?
    want_rc 0
    out=$(cd "$repo" && "$tools/expect.sh" list 2>&1)
    want_no_out "ex-list"
}

test_expect_rejects_a_missing_worktree_and_bad_usage() {
    new_fixtures
    out=$(cd "$repo" && "$tools/expect.sh" start "$work/nowhere" 2>&1); rc=$?
    want_rc 2
    out=$(cd "$repo" && "$tools/expect.sh" 2>&1); rc=$?
    want_rc 2; want_out "usage:"
}

test_watchdog_flags_an_idle_agent_with_a_report_pending() {
    new_fixtures; expect_wt ex-idle
    expect_start "$work/ex-idle" "task 7 review"
    expect_watchdog 1300
    want_rc 3; want_out "agent at ex-idle quiet for 1300s"; want_out "task 7 review"
    # A restarted watchdog alerts again while the agent is still idle.
    expect_watchdog 1400
    want_rc 3; want_out "quiet for 1400s"
}

test_watchdog_ignores_an_agent_under_the_quiet_limit() {
    new_fixtures; expect_wt ex-young
    expect_start "$work/ex-young" "task 7"
    expect_watchdog 1100
    want_rc 0
    # The limit is configurable.
    echo $((t0 + 100)) >"$fx/clock"
    out=$(cd "$repo" && env AGENTIC_NOW_CMD=fake-now AGENTIC_WATCHDOG_QUIET=50 \
        timeout 15 "$tools/watchdog.sh" --once 2>&1); rc=$?
    want_rc 3; want_out "quiet for 100s"
}

test_watchdog_ignores_an_agent_with_a_fresh_log() {
    new_fixtures; expect_wt ex-log
    expect_start "$work/ex-log" "task 7"
    mkdir -p "$work/ex-log/target/i7-logs/deep"
    touch -d "@$((t0 + 1000))" "$work/ex-log/target/i7-logs/deep/gate.log"
    expect_watchdog 1300
    want_rc 0
    expect_watchdog 2300
    want_rc 3; want_out "quiet for 1300s"
}

test_watchdog_counts_a_new_commit_as_activity() {
    new_fixtures; expect_wt ex-commit
    expect_start "$work/ex-commit" "task 7"
    # The reflog time is the real time of this commit, at or after t0.
    git_quiet -C "$work/ex-commit" commit -q --allow-empty -m work
    now=$(date +%s)
    expect_watchdog $((now - t0 + 100))
    want_rc 0
}

# A fake working process with its cwd in the worktree: start_busy <dir> <cmd...>.
start_busy() {
    fake_pids=()
    trap 'kill "${fake_pids[@]}" 2>/dev/null' EXIT
    (cd "$1" && shift && exec "$@") >/dev/null 2>&1 &
    fake_pids+=("$!")
    sleep 0.3
}

test_watchdog_ignores_an_agent_with_a_gate_waiting_for_the_lock() {
    new_fixtures; expect_wt ex-gate
    expect_start "$work/ex-gate" "task 7"
    # gate.sh waiting on the shared lock is a bash running gate.sh (then flock).
    printf '#!/usr/bin/env bash\nsleep 30\n' >"$fx/gate.sh"
    start_busy "$work/ex-gate" bash "$fx/gate.sh" lint
    expect_watchdog 3000
    want_rc 0
}

test_watchdog_ignores_an_agent_with_a_cargo_build_running() {
    new_fixtures; expect_wt ex-cargo
    expect_start "$work/ex-cargo" "task 7"
    mkdir -p "$fx/bin"; cp "$(command -v sleep)" "$fx/bin/cargo"
    mkdir -p "$work/ex-cargo/target"
    start_busy "$work/ex-cargo/target" "$fx/bin/cargo" 30
    expect_watchdog 3000
    want_rc 0
}

test_watchdog_alert_leaves_out_command_lines() {
    new_fixtures; expect_wt ex-secret
    expect_start "$work/ex-secret" "task 7"
    # An unrelated process in the worktree does not count as work, and its
    # command line (here a fake credential) never reaches the alert.
    start_busy "$work/ex-secret" bash -c 'exec -a "--token=SECRETVALUE" sleep 30'
    expect_watchdog 1300
    want_rc 3; want_out "agent at ex-secret"; want_no_out "SECRETVALUE"
}

test_watchdog_drops_an_expectation_whose_worktree_is_gone() {
    new_fixtures; expect_wt ex-gone
    expect_start "$work/ex-gone" "task 7"
    git -C "$repo" worktree remove --force "$work/ex-gone"
    expect_watchdog 5000
    want_rc 0; want_out "dropped the expectation for ex-gone"
    [ ! -e "$repo/.git/agentic-expect/ex-gone" ] || die "the entry is still there"
}

test_watchdog_stops_alerting_once_the_report_is_done() {
    new_fixtures; expect_wt ex-done
    expect_start "$work/ex-done" "task 7"
    expect_watchdog 1300
    want_rc 3
    (cd "$repo" && "$tools/expect.sh" done "$work/ex-done") >/dev/null
    expect_watchdog 1400
    want_rc 0
}

# --- runner ---

passed=0
failed=0
for test_name in $(declare -F | awk '$3 ~ /^test_/ {print $3}'); do
    t0=$(date +%s%N)
    if result=$("$test_name" 2>&1); then
        passed=$((passed + 1))
        echo "ok   $test_name ($(( ($(date +%s%N) - t0) / 1000000 )) ms)"
    else
        failed=$((failed + 1))
        echo "FAIL $test_name"
        printf '%s\n' "$result"
    fi
done
echo "$passed passed, $failed failed"
[ "$failed" = 0 ]

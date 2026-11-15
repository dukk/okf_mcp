# Common local validation commands. Keep these recipes as the stable interface
# used by contributors and issue-to-PR automation.

# apalis-postgres' query macros check a live database whenever DATABASE_URL
# is set at build time (the CLI reads that variable too). Build from its
# shipped offline cache instead.
export SQLX_OFFLINE := "true"

fmt:
    cargo fmt --all --check

lint:
    cargo clippy --all-targets -- -D warnings
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

# One Postgres server for the whole run (scripts/test-pg.sh); set
# TEST_PG_URL to reuse one already running.
test:
    scripts/test-pg.sh run cargo test

# Browser end-to-end tests of the web console (needs Docker and Chromium;
# RUSTWRIGHT_CHROMIUM names the binary if it is not on PATH).
test-ui:
    scripts/test-pg.sh run cargo test --test ui_e2e -- --test-threads=2

# The dylint policy lints (docs/contributing/policy-lints.md). Needs
# `cargo install --locked cargo-dylint dylint-link`; the first run builds the
# lint library on its own pinned nightly (minutes). `--all-targets` compiles
# test code too, so it is excluded properly. The level is warn until the
# instrument sweep finishes (M15 Task 10 makes it `-D`).
dylint:
    DYLINT_RUSTFLAGS="-W uninstrumented_fn" cargo dylint --all -- --all-targets

# Shell tests for tools/agentic (stub gh, fake clock; no Docker or network).
test-agentic:
    timeout 120 tools/agentic/tests/run.sh

# Prepare locally; ship through a PR, then publish the merged commit's tag.
release-prepare patchlevel:
    bash scripts/release-prepare.sh {{quote(patchlevel)}}

release-ship:
    bash scripts/release-ship.sh

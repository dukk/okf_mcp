## Added
- `tools/agentic/tests/run.sh` case `test_watchdog_sees_a_test_binary_in_the_build_dir_layout`.

## Changed
- `tools/agentic/watchdog.sh` recognises test binaries in cargo's newer build-dir layout (`.../debug/build/<package>/<hash>/out/<name>-<hash>`) as well as `.../debug/deps/`, and gives them the test-binary limit.

## Notes
- Issue #738, owner's first comment. The watchdog missed a lib test binary hung for over 50 minutes on 2026-11-19: the stable toolchain had moved to cargo 1.100 that morning, and with `build.build-dir` set its test binaries run from `debug/build/okf_mcp/<hash>/out/`, a path with no `/debug/deps/` in it. The `cargo test` process above it did match, but under the 4800 s gate limit, and the gate's own 75-minute (4500 s) timeout ends the run before that.

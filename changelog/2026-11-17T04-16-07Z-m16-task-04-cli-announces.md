# M16 Task 4: the CLI announces its changes (issue #557)

## Added
- `shared::announce::announce_from_cli`: connects with `Redis::connect` (either mode) and publishes an empty "check now" on the admin-generation channel. A missing variable or an unreachable Redis is a `warn` ("announcement skipped") carrying only the error kind; the URL is never logged or printed. Without `[redis]` it does nothing.
- Tests in `cli::tests`: `key revoke` reaches a subscriber as one empty message in standalone and cluster mode; a read-only command (`key list`) announces too; an unset variable and an unreachable Redis warn and the command still succeeds.

## Changed
- `dispatch` takes an `env` reader, and every command except `serve` announces after it succeeds, read-only ones included: an empty announcement costs each instance one row read, and a list of mutating commands would go stale. `serve` returns before the announcement.

## Notes
- Failed commands do not announce: the `?` returns first.
- Plan Task 4 corrected in place (raw client replaced by `Redis::subscribe`; the `env` parameter).

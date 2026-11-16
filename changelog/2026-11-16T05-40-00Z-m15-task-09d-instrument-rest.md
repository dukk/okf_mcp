# M15 Task 9d: instrument mcp, auth, mail, cli and the rest

## Added
- `#[tracing::instrument(skip_all)]` on 262 functions in `src/mcp/**`,
  `src/auth.rs`, `src/mail/**`, `src/cli.rs`, `src/cli/**`, `src/app.rs`,
  `src/catalog.rs`, `src/archive.rs`, `src/db.rs`, `src/clock*.rs`,
  `src/ntp.rs`, `src/keys.rs`, `src/keyed_lock.rs`, `src/error.rs`,
  `src/config.rs` and `src/write.rs`: `level = "debug"` on async functions and
  `pub` entry points (178), `level = "trace"` on private synchronous helpers
  (84). No span records a field, so no name, email, token, key, document text,
  path or catalog-supplied text can reach a span. `clock_check::check_once`
  takes a `debug` span like the rest: `App::check_clock_until` makes the
  `okf.clock.check` root and instruments the `check_once` future with it, so
  the function's own span is a child of that root.
- 180 `uninstrumented_fn` exemptions (179 cleared a warning; the 180th turns
  the existing span on `telemetry::scrape::serve` into an exemption), each with
  a reason:
  - 78 trivial accessors, conversions and one-line helpers;
  - 36 configuration and builder code that runs before the subscriber exists
    (`src/config.rs`, `src/telemetry/{config,mod,subscriber}.rs`);
  - 34 exporter and exporter start-up helpers in `src/telemetry/`, where a
    span would feed back into the exporter;
  - 9 formatter and layer impls in `src/telemetry/json.rs`;
  - 6 long-lived loops whose span would never close (`cli::serve`,
    `cli::cancel_on_signal`, `App::poll_until`, `App::check_clock_until`, the
    mail worker's `run_until_cancelled` and `telemetry::scrape::serve`; the
    per-attempt `okf.mail.job` root is unchanged);
  - 4 `Debug` impls and 1 `Drop` impl;
  - 3 pure matches over a closed value (`OkfError::kind`,
    `OkfError::is_internal`, `MailProvider::as_str`);
  - 2 derive hooks for `schema_with` (`close_state_schema` and the inline
    module around `MaintenanceCloseParams`);
  - 1 wrapper around the serve loop (`cli::dispatch`), and 2 functions that run
    before the subscriber exists (`main`, `cli::run`);
  - 1 per-event filter that runs inside the subscriber
    (`telemetry::subscriber::is_exportable_target`);
  - 1 function that calls `propagation::inject` (`PostmarkMailer::send`): the
    injected context must be that of the span the request is made under;
  - 1 function that carries the caller's span onto a blocking thread
    (`mcp::blocking`, which #470 changes);
  - 1 per-read adapter in the tarball read loop (`archive::Capped::read`).
- `MaintenanceCloseParams` moved into a small inline module that carries the
  exemption and is re-exported, as the plan's issue #474 note prescribes for a
  `schema_with` field. The type's path and behaviour are unchanged.

## Changed
- The dylint warning count in these paths goes from 441 to 0. Other paths keep
  their warnings until their sweep PRs merge.
- The plan's Tasks 9a-9e section has a dated "Corrected (issue #473)" note.

## Notes
- Apart from the `MaintenanceCloseParams` module move, only attributes were
  added. `OkfServer::call_tool`, `App::poll`, `send_mail` and `auth`'s
  `span_key_id` and `record_principal_ids` carry spans of their own already
  and are untouched.
- The privacy sentinel test (`tests/telemetry_privacy.rs`) is on main and passed against this tree (5 of 5).

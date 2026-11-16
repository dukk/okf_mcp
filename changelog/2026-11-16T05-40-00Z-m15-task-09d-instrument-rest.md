# M15 Task 9d: instrument mcp, auth, mail, cli and the rest

## Added
- `#[tracing::instrument(skip_all)]` on 255 functions in `src/mcp/**`,
  `src/auth.rs`, `src/mail/**`, `src/cli.rs`, `src/cli/**`, `src/app.rs`,
  `src/catalog.rs`, `src/archive.rs`, `src/db.rs`, `src/clock.rs`,
  `src/ntp.rs`, `src/keys.rs`, `src/keyed_lock.rs` and `src/write.rs`:
  `level = "debug"` on async functions and `pub` entry points (176),
  `level = "trace"` on private synchronous helpers (79). No span records a
  field, so no name, email, token, key, document text, path or catalog-supplied
  text can reach a span.
- 186 `uninstrumented_fn` exemptions, each with a reason:
  - 73 trivial accessors, conversions and one-line helpers;
  - 44 start-up code that runs before the subscriber exists (`src/config.rs`,
    the `src/telemetry/` config, builder and start-up code);
  - 34 exporter and start-up helpers in `src/telemetry/otlp.rs` and the
    telemetry shutdown path, where a span would feed back into the exporter;
  - 9 formatter and layer impls in `src/telemetry/json.rs`;
  - 9 pure error rendering and classification helpers in `src/error.rs`;
  - 4 `Debug` impls, 1 `Drop` impl;
  - 4 long-lived loops whose span would never close (`cli::serve`,
    `cli::cancel_on_signal`, `App::poll_until`, `App::check_clock_until`) and
    1 wrapper around the serve loop (`cli::dispatch`);
  - 2 functions that run before the subscriber exists (`main`, `cli::run`);
  - 1 root span maker (`clock_check::check_once`, which makes the
    `okf.clock.check` root itself);
  - 1 function that calls `propagation::inject` (`PostmarkMailer::send`): the
    injected context must be that of the span the request is made under;
  - 1 per-read adapter in the tarball read loop (`archive::Capped::read`);
  - 1 function that carries the caller's span onto a blocking thread
    (`mcp::blocking`, which #470 changes);
  - 1 derive hook (`close_state_schema`, see below).
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
- The privacy sentinel test is not yet available on main.

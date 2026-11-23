# Feedback: an operator path for failed rows, and user deletion (issue #698)

## Added
- `okf-mcp feedback list` and `okf-mcp feedback retry <id>`: list `failed`
  feedback rows (never the messages) and put one back to `pending` with a
  new queued job. `mail::feedback::list_failed` and `retry` back them.
- Migrations `0033_feedback_finished_index.sql` and
  `0034_feedback_drop_sent_index.sql` (both `no-transaction`): the prune
  index, built and the old one dropped `CONCURRENTLY`, now covers `failed`
  rows as well as `sent` ones. `feedback_finished` joins `CONCURRENT_INDEXES`
  in `db.rs`, so a failed build is dropped and rebuilt by the next `migrate`.

## Changed
- `feedback::prune` deletes `failed` rows after the same `[feedback]
  retention` as `sent` rows (it kept them forever).
- `store::users::delete` deletes the user's `pending` and `failed` feedback
  in its transaction, so no try that starts after the deletion commits
  forwards it: the job finds no row and ends as skipped. A post the worker
  claimed before the deletion is not cancelled and can still reach Forminit
  with the user's name; the deletion does not wait for it (that would hold
  the user and admin locks across a call bounded only by its 30 s timeout).
  Whatever that post's answer, it updates no row, so it is not posted
  again. `sent` rows are left: they are delivered, and the prune removes
  them.
- Migration `0035_feedback_undelivered_user_index.sql` (`no-transaction`): partial index
  `feedback_undelivered_user` on `feedback (user_slug) WHERE status IN
  ('pending', 'failed')`, built `CONCURRENTLY` and listed in
  `CONCURRENT_INDEXES`, so the user deletion's feedback delete (which runs
  under the user and admin row locks) is an index lookup. The statement is
  the const `DELETE_UNDELIVERED_FEEDBACK`, and a test checks its plan uses
  the index. `feedback list` and `retry` need no new index: `retry` is by
  primary key, `list` is an operator command over a table the prune bounds,
  and `submit`'s rate-limit count already has `feedback_credential_created`.
- `feedback::submit` takes the sender's user row `FOR KEY SHARE` first (when
  `registered`, i.e. the credential has a user) and refuses with
  `Unauthenticated` if the user is gone, so a submission cannot land after
  that user's deletion committed. `Sender` and `NewFeedback` gain
  `registered`; both entry points (`send_feedback`, console `POST
  /api/v1/feedback`) go through `submit`.
- `submit` and `retry` share one `queue_job` helper.

## Notes
- The worker already logs a warning, with the error code as `reason`, each
  time it marks a row `failed` (`feedback not delivered; marked failed`), so
  no new log was added.
- No retry path from the console or REST API: the issue allowed either the
  CLI or the console, and the CLI is the smaller change.

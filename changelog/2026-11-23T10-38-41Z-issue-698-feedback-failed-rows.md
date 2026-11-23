# Feedback: an operator path for failed rows, and user deletion (issue #698)

## Added
- `okf-mcp feedback list` and `okf-mcp feedback retry <id>`: list `failed`
  feedback rows (never the messages) and put one back to `pending` with a
  new queued job. `mail::feedback::list_failed` and `retry` back them.
- Migration `0033_feedback_failed_prune_index.sql`: the prune index now
  covers `failed` rows as well as `sent` ones.

## Changed
- `feedback::prune` deletes `failed` rows after the same `[feedback]
  retention` as `sent` rows (it kept them forever).
- `store::users::delete` deletes the user's `pending` and `failed` feedback
  in its transaction, so a deleted user's undelivered feedback is not
  forwarded with their name. `sent` rows are left: they are delivered, and
  the prune removes them.
- `submit` and `retry` share one `queue_job` helper.

## Notes
- The worker already logs a warning, with the error code as `reason`, each
  time it marks a row `failed` (`feedback not delivered; marked failed`), so
  no new log was added.
- No retry path from the console or REST API: the issue allowed either the
  CLI or the console, and the CLI is the smaller change.

//! `okf-mcp feedback list` and `retry`: the operator's path for feedback the
//! worker marked `failed` (issue #698). Each subcommand calls the same
//! `mail::feedback` function as its tests, and, like `mail`, returns the
//! text to print. A retry only queues the row again: a running server's
//! feedback worker posts it.

use sqlx::PgPool;

use super::{tab_row, FeedbackCommand};
use crate::clock::format_timestamp;
use crate::mail::feedback::{self, FailedFeedback};

/// Runs `okf-mcp feedback ...`.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `command` - which subcommand.
///
/// # Returns
/// The text to print.
///
/// # Errors
/// For `retry`, an unknown id, a row that has not failed, or a queue or
/// database error.
#[tracing::instrument(level = "debug", skip_all)]
pub(super) async fn feedback(pool: &PgPool, command: FeedbackCommand) -> anyhow::Result<String> {
    Ok(match command {
        FeedbackCommand::List => {
            let failed = feedback::list_failed(pool).await?;
            if failed.is_empty() {
                "no failed feedback\n".to_string()
            } else {
                failed.iter().map(|row| format_failed(row) + "\n").collect()
            }
        }
        FeedbackCommand::Retry { id } => {
            feedback::retry(pool, &id).await?;
            format!("requeued feedback {id}\n")
        }
    })
}

/// One `feedback list` line, tab-separated: id, `user U`, `kind K`,
/// `attempts N`, `failed at T` and `error E` (`-` for none).
///
/// # Arguments
/// * `row` - the failed feedback.
///
/// # Returns
/// The line, without a newline.
#[tracing::instrument(level = "trace", skip_all)]
fn format_failed(row: &FailedFeedback) -> String {
    tab_row(&[
        &row.id,
        &format!("user {}", row.user_slug),
        &format!("kind {}", row.kind),
        &format!("attempts {}", row.attempts),
        &format!(
            "failed at {}",
            row.finished_at
                .map_or_else(|| "-".to_string(), format_timestamp)
        ),
        &format!("error {}", row.last_error.as_deref().unwrap_or("-")),
    ])
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use clap::Parser;

    use super::feedback;
    use crate::cli::{Cli, Command, FeedbackCommand};
    use crate::db::test_db;

    /// Parses `okf-mcp feedback <args>` into its subcommand.
    fn parse(args: &[&str]) -> FeedbackCommand {
        let argv = ["okf-mcp", "feedback"]
            .into_iter()
            .chain(args.iter().copied());
        match Cli::try_parse_from(argv).map(|cli| cli.command) {
            Ok(Some(Command::Feedback(command))) => command,
            other => panic!("not a feedback command: {other:?}"),
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-11-23T12:00:00Z".parse().unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_shows_failed_rows_and_retry_requeues_one() {
        let db = test_db().await;
        sqlx::query(
            "INSERT INTO feedback (id, credential, user_slug, message, kind, origin, \
                 server_version, client, credential_kind, status, attempts, last_error, \
                 created_at, finished_at) \
             VALUES ('f1', 'key-1', 'alice', 'm', 'bug', 'user', '0', 'c', 'api_key', \
                 'failed', 1, 'FORM_NOT_FOUND', $1, $1)",
        )
        .bind(now())
        .execute(&db.pool)
        .await
        .unwrap();
        let listed = feedback(&db.pool, parse(&["list"])).await.unwrap();

        let retried = feedback(&db.pool, parse(&["retry", "f1"])).await.unwrap();
        let after = feedback(&db.pool, parse(&["list"])).await.unwrap();
        let again = feedback(&db.pool, parse(&["retry", "f1"])).await;

        assert_eq!(
            listed,
            "f1\tuser alice\tkind bug\tattempts 1\tfailed at 2026-11-23T12:00:00Z\t\
             error FORM_NOT_FOUND\n"
        );
        assert_eq!(retried, "requeued feedback f1\n");
        assert_eq!(after, "no failed feedback\n");
        assert!(again.is_err(), "a pending row is not retried");
    }
}

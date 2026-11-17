//! `okf-mcp audit list` and `audit export`: reading the audit log from the
//! command line (spec Part 5, "Who reads it, and how").
//!
//! The CLI has operator access, so it reads as [`Viewer::Admin`]: every
//! entry, unredacted, as an every-catalog admin does over REST. Both
//! subcommands parse their flags with [`audit::parse_filters`] and read
//! with [`audit::list`] / [`audit::export_lines`], the functions behind
//! `GET /api/v1/audit` and `GET /api/v1/audit/export`, so the CLI and the
//! routes refuse the same input and print the same entries.

use std::borrow::Cow;

use sqlx::PgPool;

use super::{comma_list, tab_row, AuditArgs, AuditCommand};
use crate::store::audit::{self, ActorView, AuditRecord, FilterText, TargetView, Viewer};

/// Runs `okf-mcp audit ...`.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `command` - which audit subcommand, with its filters.
///
/// # Returns
/// The text to print: for `list`, one tab-separated line per entry (see
/// [`format_record`]); for `export`, every matching entry as JSON Lines.
/// Both are newest first and empty when nothing matches.
///
/// # Errors
/// An unknown action or a time that is not RFC 3339 (naming the filter),
/// or a database error.
#[tracing::instrument(level = "debug", skip_all)]
pub(super) async fn audit(pool: &PgPool, command: &AuditCommand) -> anyhow::Result<String> {
    Ok(match command {
        AuditCommand::List(args) => {
            let query = audit::parse_filters(&filter_text(args))?;
            audit::list(pool, &Viewer::Admin, &query)
                .await?
                .iter()
                .map(|record| format_record(record) + "\n")
                .collect()
        }
        AuditCommand::Export(args) => {
            let query = audit::parse_filters(&filter_text(args))?;
            audit::export_lines(pool, &Viewer::Admin, &query).await?
        }
    })
}

/// The flags as the text filters [`audit::parse_filters`] checks.
///
/// # Arguments
/// * `args` - the flags.
///
/// # Returns
/// The filters, borrowing the flags' text.
#[tracing::instrument(level = "trace", skip_all)]
fn filter_text(args: &AuditArgs) -> FilterText<'_> {
    FilterText {
        // `as_deref` lends an `Option<String>` out as an `Option<&str>`.
        team: args.team.as_deref(),
        action: args.action.as_deref(),
        actor: args.actor.as_deref(),
        target: args.target.as_deref(),
        since: args.since.as_deref(),
        until: args.until.as_deref(),
        after: args.after,
        limit: args.limit,
    }
}

/// One `audit list` line, tab-separated: id, time, action, actor (`person
/// NAME`, else `credential ID`, else the actor label), target (`user
/// NAME`, or `KIND NAME` followed by `person NAME` when it names one),
/// teams (comma-separated, `-` for none), then the details as compact
/// JSON, last since they may be long. Control characters in names and
/// labels are escaped, so a stored tab or newline cannot shift a column or
/// forge a line.
///
/// # Arguments
/// * `record` - the entry.
///
/// # Returns
/// The line, without a newline.
#[tracing::instrument(level = "trace", skip_all)]
fn format_record(record: &AuditRecord) -> String {
    let teams: Vec<String> = record
        .teams
        .iter()
        .map(|team| escaped(team).into_owned())
        .collect();
    tab_row(&[
        &record.id.to_string(),
        &record.at,
        &record.action,
        &escaped(&format_actor(&record.actor)),
        &escaped(&format_target(&record.target)),
        comma_list(&teams).as_deref().unwrap_or("-"),
        &record.details.to_string(),
    ])
}

/// An entry's actor cell, before escaping.
///
/// # Arguments
/// * `actor` - who acted.
///
/// # Returns
/// `person NAME`, else `credential ID`, else the label; `operator` for a
/// redacted actor, which the CLI's admin view never has.
#[tracing::instrument(level = "trace", skip_all)]
fn format_actor(actor: &ActorView) -> String {
    match actor {
        ActorView::Operator => "operator".to_string(),
        ActorView::Known {
            person: Some(person),
            ..
        } => format!("person {person}"),
        ActorView::Known {
            person: None,
            credential: Some(credential),
            ..
        } => format!("credential {credential}"),
        ActorView::Known {
            person: None,
            credential: None,
            label,
            ..
        } => label.clone(),
    }
}

/// An entry's target cell, before escaping.
///
/// # Arguments
/// * `target` - what the entry is about.
///
/// # Returns
/// `user NAME` (a user is named by its person), or `KIND NAME` (`-` for no
/// name) followed by ` person NAME` when the entry names one.
#[tracing::instrument(level = "trace", skip_all)]
fn format_target(target: &TargetView) -> String {
    if target.kind == "user" {
        return format!("user {}", target.person.as_deref().unwrap_or("-"));
    }
    let named = format!("{} {}", target.kind, target.name.as_deref().unwrap_or("-"));
    match &target.person {
        Some(person) => format!("{named} person {person}"),
        None => named,
    }
}

/// `text` with every control character escaped (`\t`, `\n`, `\u{1b}`).
///
/// # Arguments
/// * `text` - a name or label.
///
/// # Returns
/// The text borrowed unchanged when it has no control character (the
/// usual case, so no copy), else an escaped copy. `Cow` ("clone on
/// write") holds either.
#[tracing::instrument(level = "trace", skip_all)]
fn escaped(text: &str) -> Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|c| {
                if c.is_control() {
                    c.escape_default().to_string()
                } else {
                    c.to_string()
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::Utc;
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};
    use crate::db::test_db;
    use crate::store::audit::{AuditActor, MAX_PAGE};
    use crate::store::users::{self, NewUser};

    /// Runs `okf-mcp audit ...` within 20 s (an export whose cursor never
    /// advances would loop forever).
    async fn run_audit(pool: &PgPool, command: AuditCommand) -> anyhow::Result<String> {
        tokio::time::timeout(Duration::from_secs(20), audit(pool, &command))
            .await
            .expect("the audit command finished within 20 s")
    }

    /// Inserts `count` `setting.changed` entries in one statement.
    ///
    /// # Returns
    /// Their ids, newest (highest) first.
    async fn setting_entries(pool: &PgPool, count: i64) -> Vec<i64> {
        let mut ids: Vec<i64> = sqlx::query_scalar(
            "INSERT INTO audit_entries (at, action, source, actor_label, actor_operator, \
                 target_kind, target_name) \
             SELECT now(), 'setting.changed', 'cli', 'cli:test', true, 'setting', 's' || n \
             FROM generate_series(1, $1) AS n RETURNING id",
        )
        .bind(count)
        .fetch_all(pool)
        .await
        .unwrap();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        ids
    }

    /// The `id` of each JSON line.
    fn line_ids(lines: &str) -> Vec<i64> {
        lines
            .lines()
            .map(|line| {
                let record: serde_json::Value = serde_json::from_str(line).unwrap();
                record["id"].as_i64().expect("an entry id")
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn audit_list_prints_one_line_per_entry_and_export_prints_json_lines() {
        let db = test_db().await;
        users::create(
            &db.pool,
            &NewUser {
                name: "alice",
                display_name: &crate::labels::DisplayName::default(),
            },
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let filters = AuditArgs {
            action: Some("user.created".into()),
            ..AuditArgs::default()
        };

        let listed = run_audit(&db.pool, AuditCommand::List(filters.clone()))
            .await
            .unwrap();
        let exported = run_audit(&db.pool, AuditCommand::Export(filters))
            .await
            .unwrap();

        // Newest first: alice, then the fixture user's `user.created`.
        let line = listed.lines().next().unwrap();
        let cells: Vec<&str> = line.split('\t').collect();
        assert_eq!(cells.len(), 7, "{line}");
        assert_eq!(cells[2], "user.created", "{line}");
        assert_eq!(cells[4], "user alice", "{line}");
        assert_eq!(cells[5], "-", "a user entry belongs to no team: {line}");
        assert_eq!(cells[6], "{}", "{line}");
        assert!(cells[3].starts_with("cli:"), "the OS user acted: {line}");
        assert_eq!(listed.lines().count(), exported.lines().count());
        let record: serde_json::Value =
            serde_json::from_str(exported.lines().next().unwrap()).unwrap();
        assert_eq!(record["source"], "cli");
        assert_eq!(record["action"], "user.created");
        assert_eq!(record["target"]["person"], "alice");
        assert_eq!(cells[0], record["id"].to_string(), "the same entry first");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn audit_export_pages_through_every_entry_once_from_any_cursor() {
        let db = test_db().await;
        // Two full pages and a partial one: the export moves its cursor twice.
        let expected = setting_entries(&db.pool, 2 * MAX_PAGE + 3).await;
        let cursor = expected[600];
        let filters = |after| AuditArgs {
            action: Some("setting.changed".into()),
            after,
            limit: Some(3),
            ..AuditArgs::default()
        };

        let all = run_audit(&db.pool, AuditCommand::Export(filters(None)))
            .await
            .unwrap();
        let from_cursor = run_audit(&db.pool, AuditCommand::Export(filters(Some(cursor))))
            .await
            .unwrap();
        let listed = run_audit(&db.pool, AuditCommand::List(filters(Some(cursor))))
            .await
            .unwrap();

        assert_eq!(line_ids(&all), expected, "every entry once, newest first");
        assert_eq!(
            line_ids(&from_cursor),
            expected[601..],
            "only older entries"
        );
        let listed_ids: Vec<&str> = listed
            .lines()
            .map(|line| line.split('\t').next().unwrap())
            .collect();
        let page: Vec<String> = expected[601..604].iter().map(i64::to_string).collect();
        assert_eq!(listed_ids, page, "list honours --after and --limit");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_bad_filter_is_refused_naming_it() {
        let db = test_db().await;
        for (args, filter) in [
            (
                AuditArgs {
                    action: Some("user.nope".into()),
                    ..AuditArgs::default()
                },
                "action",
            ),
            (
                AuditArgs {
                    since: Some("yesterday".into()),
                    ..AuditArgs::default()
                },
                "since",
            ),
        ] {
            let listed = run_audit(&db.pool, AuditCommand::List(args.clone())).await;
            let exported = run_audit(&db.pool, AuditCommand::Export(args)).await;

            for error in [listed.unwrap_err(), exported.unwrap_err()] {
                assert!(error.to_string().contains(filter), "{error}");
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn control_characters_in_names_cannot_shift_a_column_or_forge_a_line() {
        let db = test_db().await;
        sqlx::query(
            "INSERT INTO audit_entries (at, action, source, actor_label, actor_operator, \
                 target_kind, target_name) \
             VALUES (now(), 'setting.changed', 'cli', E'cli:a\\tb', true, 'setting', E'x\\ny')",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        let filters = AuditArgs {
            action: Some("setting.changed".into()),
            ..AuditArgs::default()
        };

        let listed = run_audit(&db.pool, AuditCommand::List(filters))
            .await
            .unwrap();

        assert_eq!(listed.lines().count(), 1, "{listed:?}");
        let cells: Vec<&str> = listed.trim_end().split('\t').collect();
        assert_eq!(cells.len(), 7, "{listed:?}");
        assert_eq!(cells[3], "cli:a\\tb");
        assert_eq!(cells[4], "setting x\\ny");
        assert_eq!(cells[5], "-");
    }

    #[test]
    fn audit_flags_parse() {
        let parsed = Cli::try_parse_from([
            "okf-mcp",
            "audit",
            "list",
            "--team",
            "family",
            "--action",
            "user.created",
            "--after",
            "42",
            "--limit",
            "5",
        ])
        .unwrap();

        let Some(Command::Audit(AuditCommand::List(args))) = parsed.command else {
            panic!("not `audit list`: {:?}", parsed.command);
        };
        assert_eq!(
            args,
            AuditArgs {
                team: Some("family".into()),
                action: Some("user.created".into()),
                after: Some(42),
                limit: Some(5),
                ..AuditArgs::default()
            }
        );
    }

    #[test]
    fn targets_and_actors_read_as_documented() {
        let target = |kind: &str, name: Option<&str>, person: Option<&str>| TargetView {
            kind: kind.into(),
            name: name.map(Into::into),
            person: person.map(Into::into),
        };
        let known = |person: Option<&str>, credential: Option<&str>| ActorView::Known {
            person: person.map(Into::into),
            credential: credential.map(Into::into),
            label: "api key".into(),
            operator: false,
        };

        assert_eq!(
            format_target(&target("user", None, Some("bob"))),
            "user bob"
        );
        assert_eq!(
            format_target(&target("team", Some("family"), Some("bob"))),
            "team family person bob"
        );
        assert_eq!(
            format_target(&target("catalog", Some("main"), None)),
            "catalog main"
        );
        assert_eq!(format_target(&target("key", None, None)), "key -");
        assert_eq!(
            format_actor(&known(Some("alice"), Some("k1"))),
            "person alice"
        );
        assert_eq!(format_actor(&known(None, Some("k1"))), "credential k1");
        assert_eq!(format_actor(&known(None, None)), "api key");
        assert_eq!(format_actor(&ActorView::Operator), "operator");
        assert!(matches!(escaped("plain"), Cow::Borrowed("plain")));
    }
}

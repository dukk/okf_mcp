//! `okf-mcp unlock create`, `list` and `end` (spec Part 6, "An unlock"):
//! operator access to a customer catalog's contents on a server with
//! `require_unlock` on.
//!
//! Each subcommand calls the same `store::unlocks` function as its REST
//! route (`src/http/api/unlocks.rs`). The CLI has operator access, so it
//! sees full records and names any admin user; the store still refuses a
//! user who is not an admin. Like `people`, each handler returns the text
//! to print, and a store write bumps the administrative generation, which
//! is how a running server notices it.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use super::{tab_row, UnlockCommand};
use crate::clock::format_timestamp;
use crate::config::AuthConfig;
use crate::store::audit::AuditActor;
use crate::store::settings;
use crate::store::unlocks::{self, NewUnlock, UnlockFilter, UnlockPolicy, UnlockRecord};

/// Runs `okf-mcp unlock ...`.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `defaults` - the config file's `[auth]` table, under the stored
///   settings (`unlock_max_hours`, `unlock_notify_owners`).
/// * `command` - which unlock subcommand.
/// * `now` - the server clock: an unlock's start or end, and each listed
///   unlock's state.
///
/// # Returns
/// The text to print.
///
/// # Errors
/// As `unlocks::create` (hours over the limit, a bad reason, an unknown
/// catalog or user, a user who is not an admin) and `unlocks::end` (an
/// unknown id, one already ended or expired), or a database error.
pub(super) async fn unlock(
    pool: &PgPool,
    defaults: &AuthConfig,
    command: UnlockCommand,
    now: DateTime<Utc>,
) -> anyhow::Result<String> {
    Ok(match command {
        UnlockCommand::Create {
            catalog,
            user,
            category,
            reason,
            hours,
        } => {
            let settings = settings::load(pool, defaults).await?;
            let policy = UnlockPolicy {
                max_hours: settings.unlock_max_hours,
                notify_owners: settings.unlock_notify_owners,
            };
            let new = NewUnlock {
                catalog: &catalog,
                user: &user,
                category,
                reason: &reason,
                hours: hours.unwrap_or(policy.max_hours),
            };
            // The command line has no principal: the operating-system user
            // is the actor (`AuditActor::cli`).
            let made = unlocks::create(pool, &new, &policy, &AuditActor::cli(), now).await?;
            format!(
                "unlocked {} for {} until {} (unlock {})\n",
                made.catalog,
                made.person,
                format_timestamp(made.expires_at),
                made.id
            )
        }
        UnlockCommand::List { catalog, live } => {
            let filter = UnlockFilter {
                catalog: catalog.as_deref(),
                team: None,
                live_at: live.then_some(now),
            };
            unlocks::list(pool, &filter)
                .await?
                .iter()
                .map(|record| format_unlock(record, now) + "\n")
                .collect()
        }
        UnlockCommand::End { id } => {
            unlocks::end(pool, id, &AuditActor::cli(), now).await?;
            format!("ended unlock {id}\n")
        }
    })
}

/// One `unlock list` line, tab-separated: id, catalog, user (or `deleted
/// user #N`), category, state, `reads R`, `writes W`, `exports E`, `until
/// T` (its early end, else its expiry). The free-text reason is not
/// listed: `okf-mcp audit list` shows it in `catalog.unlocked`.
///
/// # Arguments
/// * `record` - the unlock.
/// * `now` - the time its state is computed at.
///
/// # Returns
/// The line, without a newline.
fn format_unlock(record: &UnlockRecord, now: DateTime<Utc>) -> String {
    tab_row(&[
        &record.id.to_string(),
        &record.catalog,
        &record.person,
        record.category.as_str(),
        record.state(now).as_str(),
        &format!("reads {}", record.reads),
        &format!("writes {}", record.writes),
        &format!("exports {}", record.exports),
        &format!("until {}", format_timestamp(record.ends())),
    ])
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta, Utc};
    use clap::Parser;

    use super::unlock;
    use crate::cli::{Cli, Command, UnlockCommand};
    use crate::config::AuthConfig;
    use crate::db::{test_db, TEST_TEAM};
    use crate::store::audit::AuditActor;
    use crate::store::catalogs::{self, NewCatalog};
    use crate::store::users::{self, AdminChange, NewUser};

    fn now() -> DateTime<Utc> {
        "2026-11-13T12:00:00Z".parse().unwrap()
    }

    /// Parses `okf-mcp unlock <args>` into its subcommand.
    fn parse(args: &[&str]) -> Result<UnlockCommand, clap::Error> {
        let argv = ["okf-mcp", "unlock"]
            .into_iter()
            .chain(args.iter().copied());
        Cli::try_parse_from(argv).map(|cli| match cli.command {
            Some(Command::Unlock(command)) => command,
            other => panic!("not an unlock command: {other:?}"),
        })
    }

    /// Admin user `staff` and catalog `main` owned by TEST_TEAM.
    async fn staff_and_main(pool: &sqlx::PgPool) {
        let user = NewUser {
            name: "staff",
            display_name: "",
        };
        users::create(pool, &user, &AuditActor::cli(), now())
            .await
            .unwrap();
        let on = AdminChange {
            on: true,
            allow_last: true,
        };
        users::set_admin(pool, "staff", on, &AuditActor::cli(), now())
            .await
            .unwrap();
        let main = NewCatalog {
            name: "main",
            description: "",
            owner_team: TEST_TEAM,
        };
        catalogs::create(pool, &main, &AuditActor::cli(), now())
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unlock_create_list_and_end() {
        let db = test_db().await;
        staff_and_main(&db.pool).await;
        let run = |args: &'static [&'static str], at: DateTime<Utc>| {
            let pool = db.pool.clone();
            async move { unlock(&pool, &AuthConfig::default(), parse(args).unwrap(), at).await }
        };
        let create: &[&str] = &[
            "create",
            "--catalog",
            "main",
            "--user",
            "staff",
            "--category",
            "support",
            "--reason",
            "ticket 12",
            "--hours",
            "2",
        ];

        let made = run(create, now()).await.unwrap();
        let id: i64 = made
            .trim_end()
            .rsplit_once("(unlock ")
            .and_then(|(_, rest)| rest.strip_suffix(')'))
            .unwrap()
            .parse()
            .unwrap();
        let listed = run(&["list"], now()).await.unwrap();
        let live = run(&["list", "--live"], now()).await.unwrap();
        let other = run(&["list", "--catalog", "other"], now()).await.unwrap();
        let ended_at = now() + TimeDelta::minutes(30);
        let ended = run(&["end", "1"], ended_at).await;
        let after = run(&["list"], ended_at).await.unwrap();
        let none_live = run(&["list", "--live"], ended_at).await.unwrap();
        let again = run(&["end", "1"], ended_at).await;

        assert_eq!(
            made,
            format!("unlocked main for staff until 2026-11-13T14:00:00Z (unlock {id})\n")
        );
        assert_eq!(
            listed,
            format!(
                "{id}\tmain\tstaff\tsupport\tlive\treads 0\twrites 0\texports 0\t\
                 until 2026-11-13T14:00:00Z\n"
            )
        );
        assert_eq!(live, listed);
        assert_eq!(other, "");
        // The first unlock of a fresh database is id 1.
        assert_eq!(id, 1);
        assert_eq!(ended.unwrap(), "ended unlock 1\n");
        assert_eq!(
            after,
            "1\tmain\tstaff\tsupport\tended\treads 0\twrites 0\texports 0\t\
             until 2026-11-13T12:30:00Z\n"
        );
        assert_eq!(none_live, "");
        assert!(again.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unlock_create_takes_the_limit_by_default_and_refuses_past_it() {
        let db = test_db().await;
        staff_and_main(&db.pool).await;
        let base = ["create", "--catalog", "main", "--user", "staff"];
        let with = |extra: &[&str]| {
            let args: Vec<&str> = base
                .iter()
                .chain(["--category", "legal", "--reason", "case 7"].iter())
                .chain(extra.iter())
                .copied()
                .collect();
            parse(&args).unwrap()
        };

        let default = unlock(&db.pool, &AuthConfig::default(), with(&[]), now()).await;
        let too_long = unlock(
            &db.pool,
            &AuthConfig::default(),
            with(&["--hours", "5"]),
            now(),
        )
        .await;
        let category = parse(&[
            "create",
            "--catalog",
            "main",
            "--user",
            "staff",
            "--category",
            "audit",
            "--reason",
            "x",
        ]);

        // `unlock_max_hours` is 4 with no override.
        assert!(default.unwrap().contains("until 2026-11-13T16:00:00Z"));
        assert!(too_long.unwrap_err().to_string().contains("1 to 4 hours"));
        assert!(category.is_err());
    }
}

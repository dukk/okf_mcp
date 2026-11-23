//! `okf-mcp stores ...`: the catalog stores, the default store and team pins
//! (catalog stores design, "Placement and the admin surface"). Each handler
//! returns the text to print and calls the same store function as its REST
//! route (`src/http/api/stores.rs`).

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use super::StoresCommand;
use crate::clock::format_timestamp;
use crate::store::audit::AuditActor;
use crate::store::stores::{self, StoreLocation, StoreRow, StoreSummary};
use crate::store::teams;
use crate::stores::Stores;

/// Runs `okf-mcp stores ...`.
///
/// # Arguments
/// * `pool` - the migrated account database.
/// * `stores` - the registry, whose open path `add` checks a store with.
/// * `command` - which subcommand.
/// * `now` - the time a change is recorded at.
///
/// # Returns
/// The text to print.
///
/// # Errors
/// A store that cannot be reached, is not enabled yet, or is unknown, or a
/// database error.
#[tracing::instrument(level = "debug", skip_all)]
pub(super) async fn stores(
    pool: &PgPool,
    stores: &Stores,
    command: StoresCommand,
    now: DateTime<Utc>,
) -> anyhow::Result<String> {
    // The command line has no principal: the operating-system user is the
    // actor (`AuditActor::cli`).
    let actor = AuditActor::cli();
    Ok(match command {
        StoresCommand::List => stores::summaries(pool)
            .await?
            .iter()
            .map(|summary| format_store(summary) + "\n")
            .collect(),
        StoresCommand::Add {
            name,
            host,
            port,
            database,
            credential_kind,
            credential_ref,
        } => {
            let row = StoreRow {
                name: name.clone(),
                location: StoreLocation::Host {
                    host,
                    port,
                    database,
                    credential_kind,
                    credential_ref,
                },
            };
            stores::add(pool, stores, row, &actor, now).await?;
            format!("added store {name}\n")
        }
        StoresCommand::SetDefault { store } => {
            stores::set_default(pool, &store, &actor, now).await?;
            format!(
                "new catalogs now go on store {store} (existing catalogs stay where they are)\n"
            )
        }
        StoresCommand::Pin { team, store } => {
            teams::set_store(pool, &team, Some(&store), &actor, now).await?;
            format!("pinned team {team} to store {store}\n")
        }
        StoresCommand::Unpin { team } => {
            teams::set_store(pool, &team, None, &actor, now).await?;
            format!("unpinned team {team}\n")
        }
    })
}

/// One `stores list` line, tab-separated: name, `default` or `-`, where it
/// is, the credential's kind and reference (never the credential), how many
/// teams are pinned to it, and when it was added.
///
/// # Arguments
/// * `summary` - the store.
///
/// # Returns
/// The line, without a newline.
#[tracing::instrument(level = "trace", skip_all)]
fn format_store(summary: &StoreSummary) -> String {
    let (place, credential) = match &summary.row.location {
        StoreLocation::AccountDatabase => ("account database".to_string(), "-".to_string()),
        StoreLocation::Host {
            host,
            port,
            database,
            credential_kind,
            credential_ref,
        } => (
            format!("{host}:{port}/{database}"),
            format!("{} {credential_ref}", credential_kind.as_str()),
        ),
    };
    [
        summary.row.name.to_string(),
        if summary.is_default { "default" } else { "-" }.to_string(),
        place,
        credential,
        format!("{} pinned", summary.pinned_teams),
        format_timestamp(summary.created_at),
    ]
    .join("\t")
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};
    use crate::db::{test_db, TEST_TEAM};
    use crate::stores::PoolSettings;

    fn command(args: &[&str]) -> StoresCommand {
        let cli = Cli::try_parse_from(
            ["okf-mcp", "stores"]
                .into_iter()
                .chain(args.iter().copied()),
        )
        .expect("the arguments parse");
        match cli.command {
            Some(Command::Stores(command)) => command,
            other => panic!("not a stores command: {other:?}"),
        }
    }

    fn registry(pool: &PgPool) -> Stores {
        Stores::new(
            pool.clone(),
            PoolSettings {
                max_connections: 2,
                idle_timeout: std::time::Duration::from_secs(60),
                connect_timeout: std::time::Duration::from_secs(2),
            },
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_commands_list_set_default_pin_and_unpin() {
        let db = test_db().await;
        let stores = registry(&db.pool);
        let now = Utc::now();
        let run = |args: &'static [&'static str]| {
            let (pool, stores) = (db.pool.clone(), stores.clone());
            async move { super::stores(&pool, &stores, command(args), now).await }
        };

        let listed = run(&["list"]).await.unwrap();
        let set = run(&["set-default", "main"]).await.unwrap();
        let pinned = run(&["pin", TEAM_ARG, "main"]).await.unwrap();
        let unpinned = run(&["unpin", TEAM_ARG]).await.unwrap();
        let unknown = run(&["set-default", "nowhere"]).await.unwrap_err();

        assert!(listed.starts_with("main\tdefault\taccount database\t-\t0 pinned\t"));
        assert!(set.contains("existing catalogs stay where they are"));
        assert_eq!(pinned, format!("pinned team {TEST_TEAM} to store main\n"));
        assert_eq!(unpinned, format!("unpinned team {TEST_TEAM}\n"));
        assert!(unknown.to_string().contains("store"), "{unknown}");
    }

    /// [`TEST_TEAM`] as a `'static` argument.
    const TEAM_ARG: &str = TEST_TEAM;

    #[tokio::test(flavor = "multi_thread")]
    async fn adding_a_store_that_cannot_be_reached_is_refused_and_names_no_host() {
        let db = test_db().await;
        let stores = registry(&db.pool);
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
            .to_string();

        let refused = super::stores(
            &db.pool,
            &stores,
            command(&[
                "add",
                "shared-1",
                "--host",
                "127.0.0.1",
                "--port",
                &closed,
                "--database",
                "okf",
                "--credential-kind",
                "env",
                "--credential-ref",
                "NO_SUCH_VARIABLE",
            ]),
            Utc::now(),
        )
        .await
        .unwrap_err();

        let text = format!("{refused:#}");
        for private in ["127.0.0.1", closed.as_str(), "NO_SUCH_VARIABLE"] {
            assert!(!text.contains(private), "{private} leaked into {text}");
        }
        assert!(Cli::try_parse_from([
            "okf-mcp",
            "stores",
            "add",
            "x",
            "--host",
            "h",
            "--database",
            "d",
            "--credential-kind",
            "password",
            "--credential-ref",
            "r",
        ])
        .is_err());
    }
}

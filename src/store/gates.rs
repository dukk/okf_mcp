//! The access gates the key cache loads (spec Parts 5 and 6): who is a
//! customer of which catalog, who is an admin user, and the audit ids an
//! operator's access is recorded with. The cache reloads them whenever
//! `admin_generation` moves (every team, user, key and catalog write bumps
//! it), so the ids and scopes an access is admitted with are as fresh as the
//! principal itself.

use sqlx::PgPool;

use crate::auth::{AccessGates, CatalogScope};
use crate::error::OkfError;

/// Loads the gates. Creates the audit rows the maps need first
/// (`ensure_audit_rows`), so every live credential, user and owner team
/// has an audit id to record an access with.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The gates.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn load(pool: &PgPool) -> Result<AccessGates, OkfError> {
    ensure_audit_rows(pool).await?;
    // Five reads at once on pooled connections (`try_join!` is
    // `asyncio.gather` that stops at the first error).
    let (members, admins, people, credentials, catalogs) = tokio::try_join!(
        sqlx::query_as::<_, (String, String)>(
            "SELECT m.user_id, c.name FROM team_members m \
             JOIN catalogs c ON c.owner_team_id = m.team_id",
        )
        .fetch_all(pool),
        sqlx::query_scalar::<_, String>("SELECT id FROM users WHERE is_admin").fetch_all(pool),
        sqlx::query_as::<_, (String, i64)>(
            "SELECT user_id, id FROM audit_people WHERE user_id IS NOT NULL",
        )
        .fetch_all(pool),
        sqlx::query_as::<_, (String, i64)>(
            "SELECT credential_id, id FROM audit_credentials WHERE credential_id IS NOT NULL",
        )
        .fetch_all(pool),
        sqlx::query_as::<_, (String, i64, Option<i64>)>(
            "SELECT c.name, c.id, a.id FROM catalogs c \
             LEFT JOIN audit_teams a ON a.team_id = c.owner_team_id",
        )
        .fetch_all(pool),
    )?;
    let mut gates = AccessGates {
        admins: admins.into_iter().collect(),
        people: people.into_iter().collect(),
        credentials: credentials.into_iter().collect(),
        catalogs: catalogs
            .into_iter()
            .map(|(name, id, team)| (name, CatalogScope { id, team }))
            .collect(),
        ..AccessGates::default()
    };
    for (user_id, catalog) in members {
        // `entry(..).or_default()`: Python's `setdefault(key, set())`.
        gates.members.entry(user_id).or_default().insert(catalog);
    }
    Ok(gates)
}

/// Creates the audit rows the cache's maps need and do not have yet: the
/// credential row of every live key and grant, the person of every user and
/// the team row of every catalog's owner. Users and teams get theirs in the
/// transaction that creates them (migration `0014_audit_row_locks.sql`), and
/// every recorded key change makes its key's, so after the first load this
/// finds only keys and grants from before the audit log. One statement per
/// missing row, outside any transaction, so each locks one user at most
/// (`audit_credential` and `audit_person` hold the owner `FOR KEY SHARE`)
/// and none can deadlock with a deletion; they create nothing for a user
/// deleted meanwhile.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Errors
/// [`OkfError::Database`].
async fn ensure_audit_rows(pool: &PgPool) -> Result<(), OkfError> {
    // A credential's label is its `actor`, which it keeps for life, as
    // `audit::record` passes it.
    let credentials: Vec<(String, String)> = sqlx::query_as(
        "SELECT k.id, k.actor FROM api_keys k WHERE k.revoked_at IS NULL \
           AND NOT EXISTS (SELECT 1 FROM audit_credentials a WHERE a.credential_id = k.id)",
    )
    .fetch_all(pool)
    .await?;
    for (id, label) in credentials {
        sqlx::query("SELECT audit_credential($1, NULL, $2)")
            .bind(id)
            .bind(label)
            .execute(pool)
            .await?;
    }
    let users: Vec<String> = sqlx::query_scalar(
        "SELECT u.id FROM users u \
         WHERE NOT EXISTS (SELECT 1 FROM audit_people p WHERE p.user_id = u.id)",
    )
    .fetch_all(pool)
    .await?;
    for user_id in users {
        sqlx::query("SELECT audit_person($1)")
            .bind(user_id)
            .execute(pool)
            .await?;
    }
    let teams: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT c.owner_team_id FROM catalogs c WHERE c.owner_team_id IS NOT NULL \
           AND NOT EXISTS (SELECT 1 FROM audit_teams a WHERE a.team_id = c.owner_team_id)",
    )
    .fetch_all(pool)
    .await?;
    for team_id in teams {
        sqlx::query("SELECT audit_team_ref($1)")
            .bind(team_id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{test_db, TEST_USER};

    #[tokio::test(flavor = "multi_thread")]
    async fn a_key_from_before_the_audit_log_gets_its_credential_id_at_load() {
        let db = test_db().await;
        // A row inserted directly, as a pre-M14 key: no audit row yet.
        sqlx::query(
            "INSERT INTO api_keys (id, name, sha256, access, all_catalogs, actor, created_at, \
                 user_id) \
             VALUES ('legacy', 'legacy', sha256('legacy'::bytea), 'admin', true, 'laptop', \
                     now(), (SELECT id FROM users WHERE name = $1))",
        )
        .bind(TEST_USER)
        .execute(&db.pool)
        .await
        .unwrap();

        let gates = load(&db.pool).await.unwrap();

        let (id, label): (i64, Option<String>) = sqlx::query_as(
            "SELECT id, label FROM audit_credentials WHERE credential_id = 'legacy'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(gates.credentials.get("legacy"), Some(&id));
        assert_eq!(label.as_deref(), Some("laptop"));
        let me: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(TEST_USER)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert!(gates.people.contains_key(&me));
        assert!(!gates.members.contains_key(&me), "no catalog yet");
    }
}

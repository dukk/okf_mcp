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
/// has an audit id to record an access with; then reads every map in one
/// read-only `REPEATABLE READ` transaction, so memberships, admins and
/// catalog scopes describe one moment: never a catalog's replacement (or
/// new owner) beside its old members, which would admit an admin read as
/// a customer's.
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
    load_with(pool, || async {}).await
}

/// [`load`], running `between` after the first read of the snapshot and
/// before the others: the seam a test uses to change the data mid-load.
///
/// # Errors
/// [`OkfError::Database`].
// `F: FnOnce() -> Fut` with `Fut: Future`: a closure returning a future,
// the Rust spelling of "an async callback".
async fn load_with<F, Fut>(pool: &PgPool, between: F) -> Result<AccessGates, OkfError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    ensure_audit_rows(pool).await?;
    let mut tx = pool.begin().await?;
    // Must be the transaction's first statement; the snapshot is taken by
    // the first query after it, and every later one reads the same.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let members: Vec<(String, String)> = sqlx::query_as(
        "SELECT m.user_id, c.name FROM team_members m \
         JOIN catalogs c ON c.owner_team_id = m.team_id",
    )
    .fetch_all(&mut *tx)
    .await?;
    between().await;
    let admins: Vec<String> = sqlx::query_scalar("SELECT id FROM users WHERE is_admin")
        .fetch_all(&mut *tx)
        .await?;
    let people: Vec<(String, i64)> =
        sqlx::query_as("SELECT user_id, id FROM audit_people WHERE user_id IS NOT NULL")
            .fetch_all(&mut *tx)
            .await?;
    let credentials: Vec<(String, i64)> = sqlx::query_as(
        "SELECT credential_id, id FROM audit_credentials WHERE credential_id IS NOT NULL",
    )
    .fetch_all(&mut *tx)
    .await?;
    let catalogs: Vec<(String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT c.name, c.id, a.id FROM catalogs c \
         LEFT JOIN audit_teams a ON a.team_id = c.owner_team_id",
    )
    .fetch_all(&mut *tx)
    .await?;
    // Nothing was written; ending the transaction frees its connection.
    tx.commit().await?;
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

    #[tokio::test(flavor = "multi_thread")]
    async fn the_gates_are_one_snapshot_across_a_catalog_replaced_mid_load() {
        use crate::store::audit::AuditActor;
        use crate::store::catalogs::{self, NewCatalog};
        let db = test_db().await;
        let cli = AuditActor::cli();
        let main = |owner_team| NewCatalog {
            name: "main",
            description: "",
            owner_team,
        };
        catalogs::create(&db.pool, &main(TEST_USER), &cli, chrono::Utc::now())
            .await
            .unwrap();
        let bob = crate::store::users::NewUser {
            name: "bob",
            display_name: "",
        };
        crate::store::users::create(&db.pool, &bob, &cli, chrono::Utc::now())
            .await
            .unwrap();
        let original = catalogs::find(&db.pool, "main").await.unwrap().id;
        let me: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(TEST_USER)
            .fetch_one(&db.pool)
            .await
            .unwrap();

        // `main` is deleted and created again for bob's personal team after
        // the memberships are read and before the catalogs are.
        let gates = load_with(&db.pool, || async {
            catalogs::delete(&db.pool, "main", true, &cli, chrono::Utc::now())
                .await
                .unwrap();
            catalogs::create(&db.pool, &main("bob"), &cli, chrono::Utc::now())
                .await
                .unwrap();
        })
        .await
        .unwrap();

        // One moment: the old `main` and the test user's membership of it,
        // never the replacement beside that membership.
        let member = gates
            .members
            .get(&me)
            .is_some_and(|names| names.contains("main"));
        assert!(member);
        assert_eq!(gates.catalogs["main"].id, original, "{gates:?}");
    }
}

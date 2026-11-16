//! The access gates the key cache loads (spec Parts 5 and 6): who is a
//! customer of which catalog, who is an admin user, whether operator staff
//! need an unlock and which unlocks are live, which catalogs keep the
//! maintenance runner out, and the audit ids an operator's access is
//! recorded with. The cache reloads them whenever `admin_generation` moves
//! (every team, user, key, catalog, setting and unlock write bumps it), so
//! the ids and scopes an access is admitted with are as fresh as the
//! principal itself.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::auth::{AccessGates, CatalogScope};
use crate::config::AuthConfig;
use crate::error::OkfError;
use crate::store::{settings, unlocks};

/// Loads the gates. Creates the audit rows the maps need first
/// (`ensure_audit_rows`), so every live credential, user and owner team
/// has an audit id to record an access with; then reads every map in one
/// read-only `REPEATABLE READ` transaction, so memberships, entitlements,
/// admins, unlocks, the `require_unlock` setting and catalog scopes
/// describe one moment: never a catalog's replacement (or new owner)
/// beside its old members, which would admit an admin read as a
/// customer's.
///
/// # Arguments
/// * `pool` - the database.
/// * `now` - the server clock: unlocks that ended or expired by then are
///   left out (the rest are checked again on every request).
///
/// # Returns
/// The gates.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "gates.load"))]
pub async fn load(pool: &PgPool, now: DateTime<Utc>) -> Result<AccessGates, OkfError> {
    load_with(pool, now, || async {}).await
}

/// [`load`], running `between` after the first read of the snapshot and
/// before the others: the seam a test uses to change the data mid-load.
///
/// # Errors
/// [`OkfError::Database`].
// `F: FnOnce() -> Fut` with `Fut: Future`: a closure returning a future,
// the Rust spelling of "an async callback".
#[tracing::instrument(level = "trace", skip_all, fields(db.system.name = "postgresql", db.operation.name = "gates.load_with"))]
async fn load_with<F, Fut>(
    pool: &PgPool,
    now: DateTime<Utc>,
    between: F,
) -> Result<AccessGates, OkfError>
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
    let entitled: Vec<(String, String)> = sqlx::query_as(
        "SELECT e.user_id, c.name FROM user_catalogs e JOIN catalogs c ON c.id = e.catalog_id",
    )
    .fetch_all(&mut *tx)
    .await?;
    let maintenance_off: Vec<String> =
        sqlx::query_scalar("SELECT name FROM catalogs WHERE NOT maintenance")
            .fetch_all(&mut *tx)
            .await?;
    let live = unlocks::live(&mut *tx, now).await?;
    // The unlock settings have no `[auth]` default, so the defaults passed
    // here play no part in `require_unlock`.
    let require_unlock = settings::load(&mut *tx, &AuthConfig::default())
        .await?
        .require_unlock;
    // Nothing was written; ending the transaction frees its connection.
    tx.commit().await?;
    let mut gates = AccessGates {
        require_unlock,
        maintenance_off: maintenance_off.into_iter().collect(),
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
    for (user_id, catalog) in entitled {
        gates.entitled.entry(user_id).or_default().insert(catalog);
    }
    for unlock in live {
        // `clone`: the map's key is the user id, which the unlock keeps too.
        gates
            .unlocks
            .entry(unlock.user_id.clone())
            .or_default()
            .push(unlock);
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
#[tracing::instrument(level = "trace", skip_all, fields(db.system.name = "postgresql", db.operation.name = "gates.ensure_audit_rows"))]
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
    use std::collections::HashSet;

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

        let gates = load(&db.pool, Utc::now()).await.unwrap();

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
        let gates = load_with(&db.pool, Utc::now(), || async {
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

    #[tokio::test(flavor = "multi_thread")]
    async fn the_gates_hold_entitlements_live_unlocks_opt_outs_and_require_unlock() {
        use crate::store::audit::AuditActor;
        use crate::store::catalogs::{self, NewCatalog};
        use crate::store::ceiling::{self, NewEntitlement};
        use crate::store::keys::Access;
        use crate::store::settings::SettingKey;
        use crate::store::unlocks::{Category, NewUnlock, UnlockPolicy};
        use crate::store::users::{self, AdminChange, NewUser};
        let db = test_db().await;
        let (cli, now) = (AuditActor::cli(), Utc::now());
        for name in ["main", "quiet"] {
            let new = NewCatalog {
                name,
                description: "",
                owner_team: TEST_USER,
            };
            catalogs::create(&db.pool, &new, &cli, now).await.unwrap();
        }
        for name in ["bob", "staff"] {
            let user = NewUser {
                name,
                display_name: "",
            };
            users::create(&db.pool, &user, &cli, now).await.unwrap();
        }
        let admin = AdminChange {
            on: true,
            allow_last: false,
        };
        users::set_admin(&db.pool, "staff", admin, &cli, now)
            .await
            .unwrap();
        let entitlement = NewEntitlement {
            user: "bob",
            catalog: "main",
            access: Access::Read,
        };
        ceiling::entitle(&db.pool, &entitlement, &cli, now)
            .await
            .unwrap();
        let policy = UnlockPolicy {
            max_hours: 4,
            notify_owners: false,
        };
        let unlock = |catalog| NewUnlock {
            catalog,
            user: "staff",
            category: Category::Support,
            reason: "ticket 12",
            hours: 1,
        };
        let live = crate::store::unlocks::create(&db.pool, &unlock("main"), &policy, &cli, now)
            .await
            .unwrap();
        // Made two hours ago for one hour: expired.
        let two_hours_ago = now - chrono::TimeDelta::hours(2);
        crate::store::unlocks::create(&db.pool, &unlock("quiet"), &policy, &cli, two_hours_ago)
            .await
            .unwrap();
        sqlx::query("UPDATE catalogs SET maintenance = false WHERE name = 'quiet'")
            .execute(&db.pool)
            .await
            .unwrap();
        crate::store::settings::set(&db.pool, SettingKey::RequireUnlock, "on", &cli, now)
            .await
            .unwrap();
        let id_of = |name: &'static str| {
            sqlx::query_scalar::<_, String>("SELECT id FROM users WHERE name = $1").bind(name)
        };
        let bob = id_of("bob").fetch_one(&db.pool).await.unwrap();
        let staff = id_of("staff").fetch_one(&db.pool).await.unwrap();

        let gates = load(&db.pool, now).await.unwrap();

        assert!(gates.require_unlock);
        assert_eq!(gates.entitled[&bob], HashSet::from(["main".to_string()]));
        assert_eq!(gates.maintenance_off, HashSet::from(["quiet".to_string()]));
        let unlocks: Vec<(i64, &str)> = gates.unlocks[&staff]
            .iter()
            .map(|unlock| (unlock.id, unlock.catalog.as_str()))
            .collect();
        assert_eq!(unlocks, [(live.id, "main")], "the expired one is left out");
    }
}

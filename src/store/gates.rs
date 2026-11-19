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

/// Loads the gates. Writes nothing: every user, team and live credential
/// already has the audit row its access is recorded with, made in the
/// transaction that created it (or by `migrations/0022_audit_backfill.sql`
/// for older ones; M18 spec Decision 20). Reads every map in one
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use crate::db::{test_db, TEST_USER};

    /// The row counts of the three audit tables a load used to write.
    async fn audit_row_counts(pool: &PgPool) -> (i64, i64, i64) {
        sqlx::query_as(
            "SELECT (SELECT count(*) FROM audit_people), (SELECT count(*) FROM audit_teams), \
                    (SELECT count(*) FROM audit_credentials)",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_new_user_key_grant_and_team_has_its_audit_row() {
        use crate::labels::DisplayName;
        use crate::store::audit::{testing::credential_row, AuditActor};
        use crate::store::keys::{self, Access, Grants, NewKey, NewProvisioner};
        use crate::store::teams::{self, NewTeam};
        use crate::store::users::{self, NewUser};
        let db = test_db().await;
        let (cli, now) = (AuditActor::cli(), Utc::now());
        let unset = DisplayName::default();

        users::create(
            &db.pool,
            &NewUser {
                name: "bob",
                display_name: &unset,
            },
            &cli,
            now,
        )
        .await
        .unwrap();
        let team = NewTeam {
            name: "family",
            display_name: &unset,
            owner: "bob",
        };
        teams::create(&db.pool, &team, &cli, now).await.unwrap();
        let key = |name, self_issued| NewKey {
            name,
            access: Access::Read,
            maintenance_report: false,
            grants: Grants::All,
            default_catalog: None,
            actor: "laptop",
            user: "bob",
            self_issued,
            expires_at: None,
            levels: &[],
        };
        let plain = keys::create_with_id(&db.pool, &key("plain", false), &cli, now)
            .await
            .unwrap();
        // A self-issued key may not reach every catalog: give it one.
        crate::store::catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "main",
                display_name: &unset,
                description: "",
                owner_team: "family",
            },
            &cli,
            now,
        )
        .await
        .unwrap();
        let main = ["main".to_string()];
        let own = NewKey {
            grants: Grants::Catalogs(&main),
            ..key("own", true)
        };
        let own = keys::create_self_issued(&db.pool, &own, 5, &cli, now)
            .await
            .unwrap();
        let provisioner = NewProvisioner {
            name: "ci",
            actor: "laptop",
            user: "bob",
        };
        let provisioner = keys::create_provisioner(&db.pool, &provisioner, &cli, now)
            .await
            .unwrap();

        // Every row exists before any `gates::load`, which writes none.
        let (people, teams_with_row): (Vec<String>, Vec<String>) = (
            sqlx::query_scalar(
                "SELECT u.name FROM users u JOIN audit_people p ON p.user_id = u.id \
                 ORDER BY u.name",
            )
            .fetch_all(&db.pool)
            .await
            .unwrap(),
            sqlx::query_scalar(
                "SELECT t.name FROM teams t JOIN audit_teams a ON a.team_id = t.id \
                 ORDER BY t.name",
            )
            .fetch_all(&db.pool)
            .await
            .unwrap(),
        );
        assert_eq!(people, ["bob", TEST_USER]);
        assert_eq!(teams_with_row, ["bob", "family", TEST_USER]);
        let bob: String = sqlx::query_scalar("SELECT id FROM users WHERE name = 'bob'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        // Labelled with its actor, as `audit::record` labels it when it acts.
        for id in [&plain.id, &own.id, &provisioner.id] {
            assert_eq!(
                credential_row(&db.pool, id).await,
                Some((Some("laptop".to_string()), Some(bob.clone()))),
                "{id}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gates_load_writes_nothing() {
        let db = test_db().await;
        // A row inserted directly, as a key from before the audit log that
        // the backfill migration did not see: the load no longer makes its
        // audit row (M18 spec Decision 20), so it has no credential id.
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
        let before = audit_row_counts(&db.pool).await;

        let gates = load(&db.pool, Utc::now()).await.unwrap();

        assert_eq!(audit_row_counts(&db.pool).await, before);
        assert!(!gates.credentials.contains_key("legacy"));
        let me: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(TEST_USER)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert!(gates.people.contains_key(&me), "made by `users::create`");
        assert!(!gates.members.contains_key(&me), "no catalog yet");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_gates_are_one_snapshot_across_a_catalog_replaced_mid_load() {
        use crate::store::audit::AuditActor;
        use crate::store::catalogs::{self, NewCatalog};
        let db = test_db().await;
        let cli = AuditActor::cli();
        // A local the closure borrows: a temporary made inside it would be
        // dropped when it returns, leaving the struct's reference dangling.
        let unset = crate::labels::DisplayName::default();
        let main = |owner_team| NewCatalog {
            name: "main",
            display_name: &unset,
            description: "",
            owner_team,
        };
        catalogs::create(&db.pool, &main(TEST_USER), &cli, chrono::Utc::now())
            .await
            .unwrap();
        let bob = crate::store::users::NewUser {
            name: "bob",
            display_name: &crate::labels::DisplayName::default(),
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
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: TEST_USER,
            };
            catalogs::create(&db.pool, &new, &cli, now).await.unwrap();
        }
        for name in ["bob", "staff"] {
            let user = NewUser {
                name,
                display_name: &crate::labels::DisplayName::default(),
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

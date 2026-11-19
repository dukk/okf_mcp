//! The access gates of one user (M18 spec Decision 9): which teams they are
//! in, which catalogs they are entitled to, whether they are an admin, their
//! live unlocks, and the audit id their access is recorded with; and the
//! audit id of one credential. The credential cache
//! ([`crate::cache::credentials`]) loads them on demand and drops them on a
//! `user` or `team` invalidation, so the ids an access is admitted with are
//! as fresh as the principal itself. The catalog's side of each check (its
//! owner team, suspension, maintenance opt-out and audit team) comes from
//! the catalog directory's row at the moment of the check.
//!
//! Every catalog is named here by its `catalogs.id`, never by its name: a
//! catalog deleted and created again under its name is another catalog, and
//! an entitlement or unlock of the old one must never open the new one.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::error::OkfError;

/// One live unlock of a user's, as an admission checks it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct UserUnlock {
    /// The unlock's id.
    pub id: i64,
    /// The unlocked catalog's `catalogs.id`.
    pub catalog_id: i64,
    /// When it expires; checked again on every request.
    pub expires_at: DateTime<Utc>,
}

/// What a user's admissions depend on, besides the catalog's own row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserGates {
    /// Whether the user is an admin.
    pub is_admin: bool,
    /// The `teams.id` of every team the user is in (personal team
    /// included): a member of a catalog's owner team is its customer.
    pub teams: HashSet<String>,
    /// The `catalogs.id` of every catalog the user is entitled to
    /// (`user_catalogs`). An entitled non-admin is a customer; an admin
    /// user's entitlement is not (spec "Who needs an unlock").
    pub entitled: HashSet<i64>,
    /// The user's live unlocks.
    pub unlocks: Vec<UserUnlock>,
    /// The user's `audit_people` id, fixed at admission (M14 Decision 21).
    pub person: Option<i64>,
}

/// One user's gates, read inside the caller's transaction (so they describe
/// the same moment as the rest of what it reads).
///
/// # Arguments
/// * `conn` - the open transaction's connection.
/// * `user_id` - the user's id.
/// * `is_admin` - the user's admin flag, from the same transaction.
/// * `now` - the server clock: unlocks that ended or expired by then are
///   left out (the rest are checked again on every request).
///
/// # Returns
/// The gates; empty sets for a user who has none (or no longer exists).
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "gates.for_user"))]
pub async fn for_user(
    conn: &mut PgConnection,
    user_id: &str,
    is_admin: bool,
    now: DateTime<Utc>,
) -> Result<UserGates, OkfError> {
    // `&mut *conn` reborrows the connection for one query at a time.
    let teams: Vec<String> =
        sqlx::query_scalar("SELECT team_id FROM team_members WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(&mut *conn)
            .await?;
    let entitled: Vec<i64> =
        sqlx::query_scalar("SELECT catalog_id FROM user_catalogs WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(&mut *conn)
            .await?;
    let unlocks = sqlx::query_as::<_, UserUnlock>(
        "SELECT id, catalog_id, expires_at FROM unlocks \
         WHERE user_id = $1 AND ended_at IS NULL AND expires_at > $2 ORDER BY id",
    )
    .bind(user_id)
    .bind(now)
    .fetch_all(&mut *conn)
    .await?;
    let person: Option<i64> = sqlx::query_scalar("SELECT id FROM audit_people WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(UserGates {
        is_admin,
        teams: teams.into_iter().collect(),
        entitled: entitled.into_iter().collect(),
        unlocks,
        person,
    })
}

/// A key's or grant's `audit_credentials` id. Writes nothing: every live
/// credential has its row from the transaction that created it (M18 spec
/// Decision 20).
///
/// # Arguments
/// * `pool` - the database.
/// * `key_id` - its `api_keys.id`.
///
/// # Returns
/// The id, or `None` for a credential without a row.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "gates.audit_credential_id"))]
pub async fn audit_credential_id(pool: &PgPool, key_id: &str) -> Result<Option<i64>, OkfError> {
    Ok(
        sqlx::query_scalar("SELECT id FROM audit_credentials WHERE credential_id = $1")
            .bind(key_id)
            .fetch_optional(pool)
            .await?,
    )
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

        // Every row exists before anything reads it.
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
    async fn reading_a_users_gates_writes_nothing() {
        let db = test_db().await;
        // A row inserted directly, as a key from before the audit log that
        // the backfill migration did not see: reading it no longer makes its
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
        let me: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(TEST_USER)
            .fetch_one(&db.pool)
            .await
            .unwrap();

        let mut conn = db.pool.acquire().await.unwrap();
        let gates = for_user(&mut conn, &me, false, Utc::now()).await.unwrap();
        let credential = audit_credential_id(&db.pool, "legacy").await.unwrap();

        assert_eq!(audit_row_counts(&db.pool).await, before);
        assert_eq!(credential, None);
        assert!(gates.person.is_some(), "made by `users::create`");
        assert_eq!(gates.teams.len(), 1, "the personal team: {gates:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_users_gates_hold_its_entitlements_and_live_unlocks_by_catalog_id() {
        use crate::store::audit::AuditActor;
        use crate::store::catalogs::{self, NewCatalog};
        use crate::store::ceiling::{self, NewEntitlement};
        use crate::store::keys::Access;
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
        let id_of = |name: &'static str| {
            sqlx::query_scalar::<_, String>("SELECT id FROM users WHERE name = $1").bind(name)
        };
        let bob = id_of("bob").fetch_one(&db.pool).await.unwrap();
        let staff = id_of("staff").fetch_one(&db.pool).await.unwrap();

        let main = crate::store::catalogs::find(&db.pool, "main")
            .await
            .unwrap()
            .id;
        let mut conn = db.pool.acquire().await.unwrap();

        let bobs = for_user(&mut conn, &bob, false, now).await.unwrap();
        let staffs = for_user(&mut conn, &staff, true, now).await.unwrap();

        assert_eq!(bobs.entitled, HashSet::from([main]));
        assert!(bobs.unlocks.is_empty());
        assert!(staffs.is_admin && staffs.entitled.is_empty());
        let unlocks: Vec<(i64, i64)> = staffs
            .unlocks
            .iter()
            .map(|unlock| (unlock.id, unlock.catalog_id))
            .collect();
        assert_eq!(unlocks, [(live.id, main)], "the expired one is left out");
    }
}

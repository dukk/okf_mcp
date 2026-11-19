//! Tests of the credential and user caches (M18 Task 11). Lib tests run no
//! change-feed listener, so each test applies the messages a write sends
//! itself: [`apply_messages_of`] captures them with
//! `invalidate::testing::invalidations_of` and applies them, as the listener
//! would.

use std::future::Future;

use chrono::{TimeDelta, Utc};
use tokio::time::timeout;

use super::*;
use crate::auth::CredentialKind;
use crate::clock::{FixedClock, SystemClock};
use crate::db::{test_db, TEST_TEAM, TEST_USER};
use crate::invalidate::testing::invalidations_of;
use crate::labels::DisplayName;
use crate::mcp::testing::{local_metrics, render};
use crate::store::audit::AuditActor;
use crate::store::catalogs::{self, NewCatalog};
use crate::store::keys::{Access, Grants, NewKey};
use crate::store::teams::{self, Membership, NewTeam, Role};

/// Every wait in these tests is bounded by this.
const BOUND: Duration = Duration::from_secs(30);
const LONG: Duration = Duration::from_secs(3600);

/// Caches over `pool` that live an hour, on the server clock.
fn credentials(pool: &PgPool) -> Credentials {
    credentials_with(pool, Arc::new(SystemClock), LONG, Metrics::global())
}

fn credentials_with(
    pool: &PgPool,
    clock: Arc<dyn Clock>,
    ttl: Duration,
    metrics: Metrics,
) -> Credentials {
    let limits = CredentialLimits {
        credentials: 100,
        users: 100,
        ttl,
    };
    Credentials::new(pool.clone(), clock, limits, metrics)
}

fn view() -> AccessView {
    AccessView::default()
}

/// Creates catalog `name`, owned by `team`.
async fn catalog(pool: &PgPool, name: &str, team: &str) {
    let new = NewCatalog {
        name,
        display_name: &DisplayName::default(),
        description: "",
        owner_team: team,
    };
    catalogs::create(pool, &new, &AuditActor::cli(), Utc::now())
        .await
        .unwrap();
}

/// A key of `user` called `name` on `granted` at write.
fn new_key<'a>(
    name: &'a str,
    user: &'a str,
    granted: &'a [String],
    self_issued: bool,
) -> NewKey<'a> {
    NewKey {
        name,
        access: Access::Write,
        maintenance_report: false,
        grants: Grants::Catalogs(granted),
        default_catalog: None,
        actor: name,
        user,
        self_issued,
        expires_at: None,
        levels: &[],
    }
}

/// Stores `new` and returns its id and secret.
async fn stored(pool: &PgPool, new: &NewKey<'_>) -> (String, String) {
    let created = key_rows::create_with_id(pool, new, &AuditActor::cli(), Utc::now())
        .await
        .unwrap();
    (created.id, created.key)
}

/// Runs `write` and applies every change message it sent to `caches`, as
/// the change feed's listener would.
async fn apply_messages_of(pool: &PgPool, caches: &Credentials, write: impl Future<Output = ()>) {
    for invalidation in invalidations_of(pool, write).await {
        caches.apply(&invalidation).await;
    }
}

async fn user_id(pool: &PgPool, name: &str) -> String {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_is_found_with_one_query_and_then_from_cache() {
    let db = test_db().await;
    catalog(&db.pool, "pg", TEST_TEAM).await;
    let granted = ["pg".to_string()];
    let (id, secret) = stored(&db.pool, &new_key("reader", TEST_USER, &granted, false)).await;
    let (_provider, metrics, registry) = local_metrics();
    let caches = credentials_with(&db.pool, Arc::new(SystemClock), LONG, metrics);

    let first = caches.find(&secret, &view()).await.unwrap().unwrap();
    // Changed behind the cache's back: only a read would see it.
    sqlx::query("UPDATE api_keys SET actor = 'changed' WHERE id = $1")
        .bind(&id)
        .execute(&db.pool)
        .await
        .unwrap();
    let second = caches.find(&secret, &view()).await.unwrap().unwrap();

    assert_eq!(first.key_id, id);
    assert_eq!(second.actor, "reader", "served from the cache");
    let text = render(&registry);
    for line in [
        r#"okf_cache_requests_total{cache="credential",result="miss"} 1"#,
        r#"okf_cache_requests_total{cache="credential",result="hit"} 1"#,
        // One load, of one statement (`keys::record_by_hash`).
        r#"okf_cache_load_duration_seconds_count{cache="credential",outcome="ok"} 1"#,
    ] {
        assert!(text.contains(line), "{line} in\n{text}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_key_is_refused_after_its_credential_invalidation() {
    let db = test_db().await;
    catalog(&db.pool, "pg", TEST_TEAM).await;
    let granted = ["pg".to_string()];
    let (id, secret) = stored(&db.pool, &new_key("reader", TEST_USER, &granted, false)).await;
    let caches = credentials(&db.pool);
    assert!(caches.find(&secret, &view()).await.unwrap().is_some());

    let messages = invalidations_of(&db.pool, async {
        key_rows::revoke_by_id(&db.pool, &id, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
    })
    .await;
    // No message applied yet: the cache still holds it.
    let before = caches.find(&secret, &view()).await.unwrap();
    for message in &messages {
        caches.apply(message).await;
    }
    let after = caches.find(&secret, &view()).await.unwrap();

    assert_eq!(messages, [Invalidation::credential(&id)]);
    assert!(before.is_some(), "until its message is applied");
    assert!(after.is_none(), "refused once it is");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_self_issued_key_is_capped_by_its_users_current_ceiling() {
    let db = test_db().await;
    let cli = AuditActor::cli();
    let now = Utc::now();
    for name in ["alice", "bob"] {
        let user = crate::store::users::NewUser {
            name,
            display_name: &DisplayName::default(),
        };
        crate::store::users::create(&db.pool, &user, &cli, now)
            .await
            .unwrap();
    }
    let family = NewTeam {
        name: "family",
        display_name: &DisplayName::default(),
        owner: "bob",
    };
    teams::create(&db.pool, &family, &cli, now).await.unwrap();
    let alice_in = Membership {
        team: "family",
        user: "alice",
        role: Role::Member,
    };
    teams::set_member(&db.pool, &alice_in, &cli, now)
        .await
        .unwrap();
    catalog(&db.pool, "shared", "family").await;
    catalog(&db.pool, "solo", "alice").await;
    let granted = ["shared".to_string(), "solo".to_string()];
    let (_, mine) = stored(&db.pool, &new_key("mine", "alice", &granted, true)).await;
    let (_, issued) = stored(&db.pool, &new_key("issued", "alice", &granted, false)).await;
    let caches = credentials(&db.pool);
    let level = async |secret: &str, catalog: &str| {
        caches
            .find(secret, &view())
            .await
            .unwrap()
            .expect("a live key")
            .level(catalog)
    };
    assert_eq!(level(&mine, "shared").await, Some(Access::Write));

    // Alice leaves the team: its `Team` message drops her cached entry.
    apply_messages_of(&db.pool, &caches, async {
        teams::remove_member(&db.pool, "family", "alice", &cli, Utc::now())
            .await
            .unwrap();
    })
    .await;

    assert_eq!(level(&mine, "shared").await, None, "her reach shrank");
    assert_eq!(level(&mine, "solo").await, Some(Access::Write));
    assert_eq!(
        level(&issued, "shared").await,
        Some(Access::Write),
        "an admin-issued key keeps its grant"
    );

    // Member access read-only on her own team's catalog: capped to read.
    apply_messages_of(&db.pool, &caches, async {
        ceiling::set_member_access(
            &db.pool,
            "solo",
            ceiling::MemberAccess::Read,
            &cli,
            Utc::now(),
        )
        .await
        .unwrap();
    })
    .await;
    assert_eq!(level(&mine, "solo").await, Some(Access::Read));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_revoked_while_its_hash_lookup_is_loading_is_not_cached() {
    let db = test_db().await;
    catalog(&db.pool, "pg", TEST_TEAM).await;
    let granted = ["pg".to_string()];
    let (id, secret) = stored(&db.pool, &new_key("reader", TEST_USER, &granted, false)).await;
    let caches = credentials(&db.pool);
    let (view, held) = (view(), caches.key_hold.gate.lock().await);

    let (served, ()) = timeout(BOUND, async {
        tokio::join!(caches.find(&secret, &view), async {
            // The load has read the live row and waits at the hook.
            caches.key_hold.reached.notified().await;
            apply_messages_of(&db.pool, &caches, async {
                key_rows::revoke_by_id(&db.pool, &id, &AuditActor::cli(), Utc::now())
                    .await
                    .unwrap();
            })
            .await;
            drop(held);
        })
    })
    .await
    .expect("the held lookup and the revocation finish");
    let next = caches.find(&secret, &view).await.unwrap();

    assert!(
        served.unwrap().is_some(),
        "the request in flight may use what it loaded"
    );
    assert!(next.is_none(), "the next lookup reloads and refuses it");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_user_load_overtaken_by_a_team_invalidation_is_not_cached() {
    let db = test_db().await;
    let cli = AuditActor::cli();
    let user = crate::store::users::NewUser {
        name: "alice",
        display_name: &DisplayName::default(),
    };
    crate::store::users::create(&db.pool, &user, &cli, Utc::now())
        .await
        .unwrap();
    let alice_in = Membership {
        team: TEST_TEAM,
        user: "alice",
        role: Role::Member,
    };
    teams::set_member(&db.pool, &alice_in, &cli, Utc::now())
        .await
        .unwrap();
    let alice = user_id(&db.pool, "alice").await;
    let team: String = sqlx::query_scalar("SELECT id FROM teams WHERE name = $1")
        .bind(TEST_TEAM)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let caches = credentials(&db.pool);
    let held = caches.user_hold.gate.lock().await;

    let (served, ()) = timeout(BOUND, async {
        tokio::join!(caches.user(&alice), async {
            caches.user_hold.reached.notified().await;
            // Only the team's message: a scan finds no resident entry, so
            // the fence alone must keep the load out of the cache.
            sqlx::query(
                "DELETE FROM team_members WHERE team_id = $1 \
                 AND user_id = (SELECT id FROM users WHERE name = 'alice')",
            )
            .bind(&team)
            .execute(&db.pool)
            .await
            .unwrap();
            caches.apply(&Invalidation::team(&team)).await;
            drop(held);
        })
    })
    .await
    .expect("the held load and the invalidation finish");
    let next = caches.user(&alice).await.unwrap().unwrap();

    let served = served.unwrap().unwrap();
    assert!(served.gates.teams.contains(&team), "what it read");
    assert!(!next.gates.teams.contains(&team), "the next read is fresh");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_suspension_closes_the_catalog_for_every_cached_principal() {
    let test =
        crate::mcp::testing::app_with(&[("main", &[])], crate::config::Config::default()).await;
    let pool = test.app.pool();
    let granted = ["main".to_string()];
    let (_, secret) = stored(pool, &new_key("writer", TEST_USER, &granted, false)).await;
    // Assembled before the suspension, and kept (a request in progress).
    let writer = test.app.find_key(&secret).await.unwrap().unwrap();
    let directory = test.app.directory();
    let owner = directory.resolve("main").await.unwrap().unwrap();
    let team = owner.owner_team.clone().expect("an owner");
    assert!(writer
        .require_in(directory, Access::Read, "main")
        .await
        .is_ok());

    let messages = invalidations_of(pool, async {
        teams::suspend(pool, &team, Some("Paused."), &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
    })
    .await;
    for invalidation in messages {
        let message = crate::invalidate::Message {
            invalidation,
            sent_at_ms: Utc::now().timestamp_millis(),
        };
        test.app.apply(&message).await;
    }

    let refused = writer.require_in(directory, Access::Read, "main").await;
    let fresh = test.app.find_key(&secret).await.unwrap().unwrap();
    assert!(
        matches!(refused, Err(OkfError::Suspended { ref notice, .. }) if notice.as_deref() == Some("Paused.")),
        "{refused:?}"
    );
    assert!(matches!(
        fresh.require_in(directory, Access::Read, "main").await,
        Err(OkfError::Suspended { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_credential_is_not_cached() {
    let db = test_db().await;
    let (_provider, metrics, registry) = local_metrics();
    let caches = credentials_with(&db.pool, Arc::new(SystemClock), LONG, metrics);

    let first = caches.find("okf_no-such-key", &view()).await.unwrap();
    let second = caches.find("okf_no-such-key", &view()).await.unwrap();
    let grant = caches.find_grant("no-such-grant", &view()).await.unwrap();

    assert!(first.is_none() && second.is_none() && grant.is_none());
    assert_eq!(
        caches.by_hash.peek(&keys::hash("okf_no-such-key")).await,
        None
    );
    let text = render(&registry);
    assert!(
        text.contains(r#"okf_cache_requests_total{cache="credential",result="miss"} 2"#),
        "two lookups, two loads, in\n{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn entries_expire_after_credential_ttl() {
    let db = test_db().await;
    catalog(&db.pool, "pg", TEST_TEAM).await;
    let granted = ["pg".to_string()];
    let (id, secret) = stored(&db.pool, &new_key("reader", TEST_USER, &granted, false)).await;
    // moka's clock is the real one: a short TTL and a real wait (Task 4).
    let caches = credentials_with(
        &db.pool,
        Arc::new(SystemClock),
        Duration::from_millis(100),
        Metrics::global(),
    );
    caches.find(&secret, &view()).await.unwrap().unwrap();
    sqlx::query("UPDATE api_keys SET actor = 'changed' WHERE id = $1")
        .bind(&id)
        .execute(&db.pool)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;
    let reread = caches.find(&secret, &view()).await.unwrap().unwrap();

    assert_eq!(reread.actor, "changed", "read again after the TTL");
}

#[tokio::test(flavor = "multi_thread")]
async fn grants_are_found_by_id_only_and_keys_by_secret_only() {
    let db = test_db().await;
    catalog(&db.pool, "pg", TEST_TEAM).await;
    let granted = ["pg".to_string()];
    let (key_id, secret) = stored(&db.pool, &new_key("reader", TEST_USER, &granted, false)).await;
    let now = Utc::now();
    let client = crate::store::oauth::testing::insert_client(
        &db.pool,
        "https://client.example.com/client.json",
        "Example Client",
        now,
    )
    .await;
    let grant: String = sqlx::query_scalar(
        "INSERT INTO api_keys (name, kind, client_id, access, all_catalogs, actor, created_at, \
             user_id) \
         SELECT 'laptop.example', 'oauth', $1, 'read', true, 'Example Client', $2, id \
         FROM users WHERE name = $3 RETURNING id",
    )
    .bind(&client)
    .bind(now)
    .bind(TEST_USER)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let caches = credentials(&db.pool);

    let found = caches.find_grant(&grant, &view()).await.unwrap().unwrap();
    let key_by_id = caches.find_grant(&key_id, &view()).await.unwrap();
    let key = caches.find(&secret, &view()).await.unwrap().unwrap();

    assert_eq!(
        found.kind,
        CredentialKind::OAuth {
            client_id: Some(client),
            client_name: Some("Example Client".into())
        }
    );
    assert!(key_by_id.is_none(), "a key is not a grant");
    assert_eq!(key.kind, CredentialKind::ApiKey);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_that_expires_is_refused_on_its_clock_even_while_cached() {
    let db = test_db().await;
    catalog(&db.pool, "pg", TEST_TEAM).await;
    let granted = ["pg".to_string()];
    let now = Utc::now();
    let soon = NewKey {
        expires_at: Some(now + TimeDelta::hours(1)),
        ..new_key("soon", TEST_USER, &granted, false)
    };
    let (_, secret) = stored(&db.pool, &soon).await;

    let fresh = credentials_with(&db.pool, Arc::new(FixedClock(now)), LONG, Metrics::global());
    let later = credentials_with(
        &db.pool,
        Arc::new(FixedClock(now + TimeDelta::hours(2))),
        LONG,
        Metrics::global(),
    );

    assert!(fresh.find(&secret, &view()).await.unwrap().is_some());
    assert!(later.find(&secret, &view()).await.unwrap().is_none());
    assert_eq!(
        later.by_hash.peek(&keys::hash(&secret)).await,
        None,
        "an expired key is not even cached"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_principal_carries_its_users_gates_and_its_audit_ids() {
    let db = test_db().await;
    catalog(&db.pool, "main", TEST_TEAM).await;
    let new = NewKey {
        grants: Grants::All,
        access: Access::Read,
        ..new_key("member", TEST_USER, &[], false)
    };
    let (id, secret) = stored(&db.pool, &new).await;
    crate::store::users::set_admin(
        &db.pool,
        TEST_USER,
        crate::store::users::AdminChange {
            on: true,
            allow_last: false,
        },
        &AuditActor::cli(),
        Utc::now(),
    )
    .await
    .unwrap();
    let me = user_id(&db.pool, TEST_USER).await;
    let team: String = sqlx::query_scalar("SELECT id FROM teams WHERE name = $1")
        .bind(TEST_TEAM)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let caches = credentials(&db.pool);

    let principal = caches.find(&secret, &view()).await.unwrap().unwrap();
    let user = caches.user(&me).await.unwrap().unwrap();

    let gates = principal.facts.user.as_ref().expect("the key's user");
    assert!(gates.is_admin && gates.teams.contains(&team));
    assert!(gates.person.is_some());
    assert!(Arc::ptr_eq(gates, &user.gates), "one entry, shared");
    assert_eq!(
        principal.facts.credential,
        gates::audit_credential_id(&db.pool, &id).await.unwrap()
    );
    assert!(principal.facts.credential.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_maintenance_capability_change_reaches_the_cached_key() {
    let db = test_db().await;
    catalog(&db.pool, "pg", TEST_TEAM).await;
    let granted = ["pg".to_string()];
    let reporter = NewKey {
        maintenance_report: true,
        access: Access::Read,
        ..new_key("reporter", TEST_USER, &granted, false)
    };
    let (id, secret) = stored(&db.pool, &reporter).await;
    let caches = credentials(&db.pool);
    assert!(
        caches
            .find(&secret, &view())
            .await
            .unwrap()
            .unwrap()
            .maintenance_report
    );

    apply_messages_of(&db.pool, &caches, async {
        key_rows::set_maintenance_report_target(
            &db.pool,
            &id,
            None,
            false,
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
    })
    .await;

    let after = caches.find(&secret, &view()).await.unwrap().unwrap();
    assert!(!after.maintenance_report);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_catalog_created_again_under_its_name_is_not_reached_through_the_old_grant() {
    let db = test_db().await;
    catalog(&db.pool, "pg", TEST_TEAM).await;
    let granted = ["pg".to_string()];
    let (_, secret) = stored(&db.pool, &new_key("reader", TEST_USER, &granted, false)).await;
    let caches = credentials(&db.pool);
    assert!(caches
        .find(&secret, &view())
        .await
        .unwrap()
        .unwrap()
        .reaches("pg"));

    // The grant goes with the catalog; the new `pg` was never granted.
    apply_messages_of(&db.pool, &caches, async {
        catalogs::delete(&db.pool, "pg", true, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
        catalog(&db.pool, "pg", TEST_TEAM).await;
    })
    .await;

    let after = caches.find(&secret, &view()).await.unwrap().unwrap();
    assert!(!after.reaches("pg"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_connection_rereads_every_cached_entry() {
    let db = test_db().await;
    catalog(&db.pool, "pg", TEST_TEAM).await;
    let granted = ["pg".to_string()];
    let (id, secret) = stored(&db.pool, &new_key("reader", TEST_USER, &granted, false)).await;
    let caches = credentials(&db.pool);
    caches.find(&secret, &view()).await.unwrap().unwrap();
    // Revoked with its message lost.
    key_rows::revoke_by_id(&db.pool, &id, &AuditActor::cli(), Utc::now())
        .await
        .unwrap();
    assert!(caches.find(&secret, &view()).await.unwrap().is_some());

    caches.mark_all_stale();

    assert!(caches.find(&secret, &view()).await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cached_key_survives_a_failed_database_and_an_uncached_one_errors() {
    let db = test_db().await;
    catalog(&db.pool, "pg", TEST_TEAM).await;
    let granted = ["pg".to_string()];
    let (_, cached) = stored(&db.pool, &new_key("cached", TEST_USER, &granted, false)).await;
    let (_, other) = stored(&db.pool, &new_key("other", TEST_USER, &granted, false)).await;
    let caches = credentials(&db.pool);
    caches.find(&cached, &view()).await.unwrap().unwrap();

    db.pool.close().await;

    assert!(caches.find(&cached, &view()).await.unwrap().is_some());
    assert!(matches!(
        caches.find(&other, &view()).await,
        Err(OkfError::Database { .. })
    ));
}

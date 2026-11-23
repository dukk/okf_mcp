//! Tests that a catalog whose registry row is unprovisioned (`provisioned_at`
//! NULL, plan Task 10b and decision P11) is absent from every reader and
//! cannot be acted on by name. Nothing writes such a row yet, so each test
//! seeds one directly.

use chrono::{DateTime, TimeZone, Utc};
use sqlx::PgPool;

use super::*;
use crate::db::{test_db, TEST_TEAM, TEST_USER};
use crate::store::audit::AuditActor;
use crate::store::ceiling;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 23, 12, 0, 0).unwrap()
}

/// Inserts an unprovisioned catalog row owned by the test team, as step 1 of
/// a two-step create will, and returns its id.
async fn seed_unprovisioned(pool: &PgPool, name: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO catalogs (name, description, created_at, owner_team_id, provisioned_at) \
         SELECT $1, 'about it', $2, t.id, NULL FROM teams t WHERE t.name = $3 \
         RETURNING id",
    )
    .bind(name)
    .bind(now())
    .bind(TEST_TEAM)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Marks the row provisioned, as step 3 of a two-step create will.
async fn provision(pool: &PgPool, id: i64) {
    sqlx::query("UPDATE catalogs SET provisioned_at = $2 WHERE id = $1")
        .bind(id)
        .bind(now())
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_half_created_catalog_is_not_visible() {
    let db = test_db().await;
    let id = seed_unprovisioned(&db.pool, "half").await;
    let actor = AuditActor::cli();

    assert!(matches!(
        find(&db.pool, "half").await,
        Err(OkfError::CatalogNotSelected { .. })
    ));
    assert!(list(&db.pool).await.unwrap().is_empty());
    assert_eq!(row_by_name(&db.pool, "half").await.unwrap(), None);
    assert_eq!(row_by_id(&db.pool, id).await.unwrap(), None);
    // Nothing that finds a catalog by name can act on it either.
    assert!(matches!(
        delete(&db.pool, "half", true, &actor, now()).await,
        Err(OkfError::CatalogNotSelected { .. })
    ));
    assert!(matches!(
        set_owner(&db.pool, "half", TEST_TEAM, &actor, now()).await,
        Err(OkfError::CatalogNotSelected { .. })
    ));
    let mut conn = db.pool.acquire().await.unwrap();
    assert!(matches!(
        lock_by_name(&mut conn, "half").await,
        Err(OkfError::CatalogNotSelected { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_half_created_catalog_is_outside_every_ceiling() {
    let db = test_db().await;
    let id = seed_unprovisioned(&db.pool, "half").await;
    let user_id: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(TEST_USER)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    // An entitlement row that should not exist (nothing can name the
    // catalog to grant one): the readers still leave it out.
    sqlx::query(
        "INSERT INTO user_catalogs (user_id, catalog_id, access, created_at) \
         VALUES ($1, $2, 'write', now())",
    )
    .bind(&user_id)
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();

    let mut conn = db.pool.acquire().await.unwrap();
    let named = ceiling::ceiling_in(&mut conn, &user_id, &["half".to_string()])
        .await
        .unwrap();
    assert!(named.is_empty(), "{named:?}");
    let entitled = ceiling::entitlements_in(&mut conn, TEST_USER)
        .await
        .unwrap();
    assert!(entitled.is_empty());
    drop(conn);
    assert_eq!(
        ceiling::highest(&db.pool, &user_id).await.unwrap(),
        crate::store::keys::Access::Read,
        "the owner team's write on a half-made catalog is no ceiling"
    );
    let page = ceiling::page(&db.pool, &user_id, "", None, 10)
        .await
        .unwrap();
    assert!(page.entries.is_empty(), "{:?}", page.entries);
    let refused = ceiling::set_member_access(
        &db.pool,
        "half",
        MemberAccess::Read,
        &AuditActor::cli(),
        now(),
    )
    .await;
    assert!(matches!(refused, Err(OkfError::CatalogNotSelected { .. })));
    // The console's reads and the entitlement writes by name.
    assert!(matches!(
        ceiling::member_access(&db.pool, "half").await,
        Err(OkfError::CatalogNotSelected { .. })
    ));
    let page = ceiling::TeamPage {
        members_after: "",
        entitlements_after: "",
        limit: 10,
        lists: ceiling::TeamLists::Both,
    };
    assert!(ceiling::team_access(&db.pool, "half", None, &page)
        .await
        .is_err());
    let entitle = ceiling::NewEntitlement {
        user: TEST_USER,
        catalog: "half",
        access: crate::store::keys::Access::Read,
    };
    assert!(
        ceiling::entitle(&db.pool, &entitle, &AuditActor::cli(), now())
            .await
            .is_err()
    );
    // The details and maintenance changes by name.
    let details = DetailsChange {
        catalog: "half",
        display_name: None,
        description: Some("x"),
        owner: None,
    };
    assert!(matches!(
        update_details(&db.pool, &details, &AuditActor::cli(), now()).await,
        Err(OkfError::CatalogNotSelected { .. })
    ));
    let maintenance = MaintenanceChange {
        catalog: "half",
        to: true.into(),
        owner: None,
    };
    assert!(matches!(
        set_maintenance(&db.pool, &maintenance, &AuditActor::cli(), now()).await,
        Err(OkfError::CatalogNotSelected { .. })
    ));

    // Once provisioned, the same readers show it.
    provision(&db.pool, id).await;
    let page = ceiling::page(&db.pool, &user_id, "", None, 10)
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_half_created_catalog_is_not_listed_granted_or_repaired() {
    let db = test_db().await;
    let id = seed_unprovisioned(&db.pool, "half").await;

    let team = crate::store::teams::find(&db.pool, TEST_TEAM)
        .await
        .unwrap();
    assert!(team.catalogs.is_empty(), "{:?}", team.catalogs);
    let mut conn = db.pool.acquire().await.unwrap();
    let granted = crate::store::keys::catalog_ids(&mut conn, &["half".to_string()]).await;
    assert!(matches!(granted, Err(OkfError::CatalogNotSelected { .. })));
    drop(conn);
    // The repair would otherwise find no root index and write one, which
    // is the store side the recovery task checks for.
    let missing = crate::store::repair::missing_root_index(
        &crate::stores::AccountDb::of(&db.pool),
        &crate::stores::CatalogDb::main_of(&db.pool),
    )
    .await
    .unwrap();
    assert!(missing.is_empty(), "{missing:?}");

    provision(&db.pool, id).await;
    let team = crate::store::teams::find(&db.pool, TEST_TEAM)
        .await
        .unwrap();
    assert_eq!(team.catalogs, ["half"]);
}

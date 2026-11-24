//! Tests of the catalog events kept on the stores (catalog stores plan,
//! Task 12): the append-only guard, the prune, and the view that merges the
//! account database with every store.

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::json;
use sqlx::PgPool;

use super::testing::{stores_of, with_second_store};
use super::*;
use crate::cache::labels::LabelLookup;
use crate::db::{test_db, TEST_TEAM, TEST_USER};
use crate::store::users::{self, NewUser};
use crate::stores::{CatalogDb, StoreName, Stores};

fn at(minutes: i64) -> DateTime<Utc> {
    let base: DateTime<Utc> = "2026-11-13T12:00:00Z".parse().unwrap();
    base + TimeDelta::minutes(minutes)
}

/// A catalog named `name` in the account database, owned by `owner`.
async fn catalog(pool: &PgPool, name: &str, owner: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO catalogs (name, created_at, owner_team_id) \
         VALUES ($1, now(), (SELECT id FROM teams WHERE name = $2)) RETURNING id",
    )
    .bind(name)
    .bind(owner)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Writes one `catalog.published` entry on `store` the way a writer does:
/// the ids resolved in an account transaction, the entry in the store's.
async fn publish_entry(
    stores: &Stores,
    store: &CatalogDb,
    catalog_id: i64,
    name: &str,
    actor: &AuditActor,
    when: DateTime<Utc>,
) {
    let mut account_tx = stores.accounts().begin().await.unwrap();
    let resolved = resolve_actor(&mut account_tx, actor, catalog_id)
        .await
        .unwrap();
    let mut tx = store.begin().await.unwrap();
    let entry = CatalogEntry {
        action: Action::CatalogPublished,
        catalog_id,
        catalog: name,
        details: json!({"files": 1}),
    };
    record_catalog(&mut tx, &resolved, &entry, when)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    account_tx.commit().await.unwrap();
}

/// The view as an admin sees it.
async fn view(stores: &Stores, query: &AuditQuery<'_>) -> Vec<AuditRecord> {
    view_as(stores, &Viewer::Admin, query).await
}

async fn view_as(stores: &Stores, viewer: &Viewer, query: &AuditQuery<'_>) -> Vec<AuditRecord> {
    let lookup = LabelLookup::new(
        stores.accounts().executor().clone(),
        100,
        std::time::Duration::from_secs(60),
    );
    list(stores, viewer, query, &lookup).await.unwrap()
}

async fn user(pool: &PgPool, name: &str) -> String {
    users::create(
        pool,
        &NewUser {
            name,
            display_name: &crate::labels::DisplayName::default(),
        },
        &AuditActor::cli(),
        at(0),
    )
    .await
    .unwrap();
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// An admin's API key actor for `user_id`.
fn actor_of(user_id: &str) -> AuditActor {
    AuditActor {
        source: Source::Console,
        credential: None,
        user_id: Some(user_id.to_string()),
        label: "console session".to_string(),
        via: Via::Session,
        operator: false,
        provisioner: false,
        ip: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn store_audit_refuses_update_and_recent_delete() {
    let db = test_db().await;
    let store = db.store();
    let insert = |when: DateTime<Utc>| {
        let store = store.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "INSERT INTO catalog_audit_entries (at, catalog_id, action, source, \
                     actor_label, actor_operator, target_name) \
                 VALUES ($1, 1, 'catalog.published', 'cli', 'cli:test', true, 'pg') RETURNING id",
            )
            .bind(when)
            .fetch_one(&store)
            .await
            .unwrap()
        }
    };
    let recent = insert(Utc::now() - TimeDelta::days(1)).await;
    let old = insert(Utc::now() - TimeDelta::days(401)).await;

    let updated = sqlx::query("UPDATE catalog_audit_entries SET action = 'x.y' WHERE id = $1")
        .bind(recent)
        .execute(&store)
        .await;
    let early = sqlx::query("DELETE FROM catalog_audit_entries WHERE id = $1")
        .bind(recent)
        .execute(&store)
        .await;
    let late = sqlx::query("DELETE FROM catalog_audit_entries WHERE id = $1")
        .bind(old)
        .execute(&store)
        .await
        .unwrap();
    let truncated = sqlx::query("TRUNCATE catalog_audit_entries")
        .execute(&store)
        .await;

    assert!(updated.is_err(), "UPDATE is refused");
    assert!(early.is_err(), "a recent entry cannot be deleted");
    assert_eq!(late.rows_affected(), 1, "an entry past 400 days can");
    assert!(truncated.is_err(), "TRUNCATE is refused");
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM catalog_audit_entries")
        .fetch_one(&store)
        .await
        .unwrap();
    assert_eq!(left, 1, "the recent entry survived every attempt");
}

#[tokio::test(flavor = "multi_thread")]
async fn store_audit_prune_honours_the_passed_retention_and_its_floor() {
    let db = test_db().await;
    let store = CatalogDb::main_of(&db.pool);
    let insert = |days: i64| {
        let pool = db.pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO catalog.catalog_audit_entries (at, catalog_id, action, source, \
                     actor_label, actor_operator, target_name) \
                 VALUES (now() - make_interval(days => $1::int), 1, 'catalog.published', 'cli', \
                         'cli:test', true, 'pg')",
            )
            .bind(i32::try_from(days).unwrap())
            .execute(&pool)
            .await
            .unwrap();
        }
    };
    let count = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM catalog.catalog_audit_entries")
            .fetch_one(&db.pool)
            .await
            .unwrap()
    };
    for days in [10, 59, 61, 100] {
        insert(days).await;
    }

    // A cut-off that reaches into a 60-day retention deletes nothing.
    let reaching = prune_catalog(&store, Utc::now() - TimeDelta::days(58), 60).await;
    assert!(reaching.is_err(), "{reaching:?}");
    assert_eq!(count().await, 4, "the refused prune deleted nothing");

    // With 60 days passed, the entries older than that go, and no more.
    let removed = prune_catalog(&store, Utc::now() - TimeDelta::days(60), 60)
        .await
        .unwrap();
    assert_eq!(removed, 2, "61 and 100 days are past 60");
    assert_eq!(count().await, 2);

    // The same cut-off with a retention of 5 days is held to the 30-day
    // floor: the 59-day entry is past it, the 10-day one is not.
    let floor = prune_catalog(&store, Utc::now() - TimeDelta::days(5), 5).await;
    assert!(floor.is_err(), "{floor:?}");
    assert_eq!(count().await, 2, "the floor refused the whole statement");
    let removed = prune_catalog(&store, Utc::now() - TimeDelta::days(35), 5)
        .await
        .unwrap();
    assert_eq!(removed, 1, "only the 59-day entry is past the 30-day floor");
    assert_eq!(count().await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn entries_from_before_the_split_appear_once_in_the_view() {
    let db = test_db().await;
    let stores = stores_of(&db.pool);
    let main = stores.get(&StoreName::main()).await.unwrap();
    let id = catalog(&db.pool, "pg", TEST_TEAM).await;
    // One entry as written before the split: in the account table.
    sqlx::query(
        "INSERT INTO audit_entries (at, action, source, actor_label, actor_operator, \
             target_kind, target_name, details) \
         VALUES ($1, 'catalog.published', 'cli', 'cli:old', true, 'catalog', 'pg', \
                 '{\"files\": 7}')",
    )
    .bind(at(0))
    .execute(&db.pool)
    .await
    .unwrap();
    // One since: on the store.
    publish_entry(&stores, &main, id, "pg", &AuditActor::cli(), at(5)).await;

    let seen = view(
        &stores,
        &AuditQuery {
            action: Some(Action::CatalogPublished),
            ..AuditQuery::default()
        },
    )
    .await;

    let files: Vec<_> = seen
        .iter()
        .map(|record| record.details["files"].clone())
        .collect();
    assert_eq!(files, [json!(1), json!(7)], "each once, newest first");
    assert_eq!(seen[0].store.as_deref(), Some("main"));
    assert_eq!(
        seen[1].store, None,
        "the old one is on the account database"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_view_pages_across_two_stores_and_accounts_without_gaps_or_repeats() {
    let db = test_db().await;
    let (stores, _other) = with_second_store(&db).await;
    let main = stores.get(&StoreName::main()).await.unwrap();
    let shared = stores
        .get(&StoreName::parse("shared-1").unwrap())
        .await
        .unwrap();
    let id = catalog(&db.pool, "pg", TEST_TEAM).await;
    // Interleaved across the three sources, with ties: minutes 0, 3, 6 and 9
    // have an entry in every source, so a page size that stops inside a tie
    // must continue with the rest of it.
    for minute in [0, 1, 3, 4, 6, 7, 9] {
        sqlx::query(
            "INSERT INTO audit_entries (at, action, source, actor_label, actor_operator, \
                 target_kind, target_name) \
             VALUES ($1, 'setting.changed', 'cli', 'cli:test', true, 'setting', 's')",
        )
        .bind(at(minute))
        .execute(&db.pool)
        .await
        .unwrap();
    }
    for minute in [0, 2, 3, 5, 6, 8, 9] {
        publish_entry(&stores, &main, id, "pg", &AuditActor::cli(), at(minute)).await;
    }
    for minute in [0, 2, 3, 4, 6, 7, 9, 10] {
        publish_entry(&stores, &shared, id, "pg", &AuditActor::cli(), at(minute)).await;
    }
    // What the view must return, from the three tables themselves.
    let mut expected: Vec<(DateTime<Utc>, String, i64)> = Vec::new();
    for (at, id) in sqlx::query_as::<_, (DateTime<Utc>, i64)>(
        "SELECT at, id FROM audit_entries WHERE action = 'setting.changed'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap()
    {
        expected.push((at, String::new(), id));
    }
    for (name, store) in [("main", &main), ("shared-1", &shared)] {
        for (at, id) in
            sqlx::query_as::<_, (DateTime<Utc>, i64)>("SELECT at, id FROM catalog_audit_entries")
                .fetch_all(store.executor())
                .await
                .unwrap()
        {
            expected.push((at, name.to_string(), id));
        }
    }
    expected.sort_unstable_by(|a, b| b.cmp(a));

    for page_size in [1, 2, 3, 4, 5, 8, 100] {
        let mut seen: Vec<(DateTime<Utc>, String, i64)> = Vec::new();
        let mut after = None;
        loop {
            let page = view(
                &stores,
                &AuditQuery {
                    after: after.clone(),
                    limit: page_size,
                    // The window leaves the fixture user's entry (made now) out.
                    since: Some(at(0)),
                    until: Some(at(11)),
                    ..AuditQuery::default()
                },
            )
            .await;
            if page.is_empty() {
                break;
            }
            after = Some(page.last().unwrap().cursor.clone());
            for record in &page {
                let when: DateTime<Utc> = record.at.parse().unwrap();
                seen.push((
                    when,
                    record.store.clone().unwrap_or_default(),
                    record.id.unwrap(),
                ));
            }
        }
        assert_eq!(seen, expected, "page size {page_size}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_view_keeps_entries_after_the_last_catalog_on_a_store_is_deleted() {
    let db = test_db().await;
    let (stores, _other) = with_second_store(&db).await;
    let shared = stores
        .get(&StoreName::parse("shared-1").unwrap())
        .await
        .unwrap();
    let id = catalog(&db.pool, "pg", TEST_TEAM).await;
    publish_entry(&stores, &shared, id, "pg", &AuditActor::cli(), at(0)).await;
    let team: i64 = sqlx::query_scalar("SELECT audit_team_ref(id) FROM teams WHERE name = $1")
        .bind(TEST_TEAM)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let owner = Viewer::Owner {
        team_id: sqlx::query_scalar("SELECT id FROM teams WHERE name = $1")
            .bind(TEST_TEAM)
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        audit_team: team,
    };

    sqlx::query("DELETE FROM catalogs WHERE id = $1")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();

    let admin = view(&stores, &AuditQuery::default()).await;
    let owned = view_as(&stores, &owner, &AuditQuery::default()).await;
    for seen in [&admin, &owned] {
        let kept: Vec<_> = seen
            .iter()
            .filter(|record| record.action == "catalog.published")
            .collect();
        assert_eq!(kept.len(), 1, "{seen:?}");
        assert_eq!(kept[0].target.name.as_deref(), Some("pg"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn audit_view_keeps_entries_after_a_catalog_is_transferred() {
    let db = test_db().await;
    let stores = stores_of(&db.pool);
    let main = stores.get(&StoreName::main()).await.unwrap();
    sqlx::query(
        "INSERT INTO teams (id, name, display_name, created_at) VALUES ('team-2', 'other', '', now())",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let id = catalog(&db.pool, "pg", TEST_TEAM).await;
    publish_entry(&stores, &main, id, "pg", &AuditActor::cli(), at(0)).await;
    let first_team = |name: &'static str| {
        let pool = db.pool.clone();
        async move {
            let (team_id, audit_team): (String, i64) =
                sqlx::query_as("SELECT id, audit_team_ref(id) FROM teams WHERE name = $1")
                    .bind(name)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            Viewer::Owner {
                team_id,
                audit_team,
            }
        }
    };
    let before = first_team(TEST_TEAM).await;
    let after = first_team("other").await;

    sqlx::query("UPDATE catalogs SET owner_team_id = 'team-2' WHERE id = $1")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    publish_entry(&stores, &main, id, "pg", &AuditActor::cli(), at(1)).await;

    let query = AuditQuery {
        action: Some(Action::CatalogPublished),
        ..AuditQuery::default()
    };
    let old_owner = view_as(&stores, &before, &query).await;
    let new_owner = view_as(&stores, &after, &query).await;
    assert_eq!(
        old_owner.len(),
        1,
        "the old owner keeps what it saw: {old_owner:?}"
    );
    assert_eq!(
        new_owner.len(),
        1,
        "the new owner sees only what came after"
    );
    assert_eq!(
        old_owner[0].at,
        at(0).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn forgetting_a_person_clears_their_name_in_store_entries_too() {
    let db = test_db().await;
    let stores = stores_of(&db.pool);
    let main = stores.get(&StoreName::main()).await.unwrap();
    let alice = user(&db.pool, "alice").await;
    let id = catalog(&db.pool, "pg", TEST_TEAM).await;
    publish_entry(&stores, &main, id, "pg", &actor_of(&alice), at(0)).await;
    let query = AuditQuery {
        action: Some(Action::CatalogPublished),
        ..AuditQuery::default()
    };
    let named = |records: &[AuditRecord]| match &records[0].actor {
        ActorView::Known { person, .. } => person.clone(),
        ActorView::Operator => None,
    };
    let before = view(&stores, &query).await;
    assert_eq!(named(&before).as_deref(), Some("alice"));

    let mut tx = db.pool.begin().await.unwrap();
    forget_person(&mut tx, &alice).await.unwrap();
    tx.commit().await.unwrap();

    let after = view(&stores, &query).await;
    let person = named(&after).expect("the entry still names a person");
    assert!(person.starts_with("deleted user #"), "{person}");
    let stored: String =
        sqlx::query_scalar("SELECT row_to_json(e)::text FROM catalog.catalog_audit_entries e")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        !stored.contains("alice"),
        "the store never held the name: {stored}"
    );
    let _ = TEST_USER;
}

#[test]
fn a_bare_entry_id_is_not_a_readable_cursor() {
    for text in [
        "607",
        "1:2",
        "x::1",
        "1::x",
        "1:Bad Store:2",
        "1:main:-3",
        "",
    ] {
        let error = AuditCursor::parse_plain(text).unwrap_err();
        assert!(
            matches!(error, OkfError::InvalidPath { ref path, .. } if path == "after"),
            "{text:?}: {error:?}"
        );
    }
    let cursor = AuditCursor::new(at(0), "shared-1", 7);
    assert_eq!(
        AuditCursor::parse_plain(&cursor.to_plain()).unwrap(),
        cursor
    );
}

//! Lock order between the changeset writers that record an audit entry
//! (publish, an admin's discard, an audited edit) and `users::delete`
//! (issue #418). Each writer takes the users its entry resolves before its
//! changeset, as the deletion does, so the two wait for each other instead
//! of deadlocking. Each forced test holds a row the writer needs after its
//! changeset lock, queues the deletion behind it, then releases the row.

use super::*;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use crate::config::Config;
use crate::mcp::testing::{app_with, cached_principal, changeset_row, context, open, TestApp};
use crate::mcp::writes::{replace_body, ReplaceBodyParams};
use crate::store::audit::AuditActor;
use crate::store::changesets::ChangesetState;
use crate::store::keys::{self, Grants, NewKey};
use crate::store::users::{self, AdminChange, DeletedUser, NewUser};

/// The longest any one wait in these tests may take.
const LOCK_WAIT: Duration = Duration::from_secs(30);

const NOTE: &str = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n";

async fn pg() -> TestApp {
    app_with(&[("pg", &[("a.md", NOTE)])], Config::default()).await
}

/// Creates user `name` (an admin user if `admin`) with an every-catalog
/// key at `access` defaulting to `pg`, polls it in, and returns the
/// context of a connection with that key.
async fn user_context(test: &TestApp, name: &str, access: Access, admin: bool) -> ToolContext {
    let pool = test.app.pool();
    let cli = AuditActor::cli();
    let user = NewUser {
        name,
        display_name: &crate::labels::DisplayName::parse(name).unwrap(),
    };
    users::create(pool, &user, &cli, Utc::now())
        .await
        .expect("create the user");
    if admin {
        let change = AdminChange {
            on: true,
            allow_last: true,
        };
        users::set_admin(pool, name, change, &cli, Utc::now())
            .await
            .expect("make the user an admin");
    }
    let key_name = format!("{name}-key");
    let new = NewKey {
        name: &key_name,
        access,
        maintenance_report: false,
        grants: Grants::All,
        default_catalog: Some("pg"),
        actor: &key_name,
        user: name,
        self_issued: false,
        expires_at: None,
        levels: &[],
    };
    let secret = keys::create_with_id(pool, &new, &cli, Utc::now())
        .await
        .expect("create the key")
        .key;
    test.app.poll().await.expect("poll the key in");
    context(test, cached_principal(test, &secret), None)
}

async fn body(ctx: &ToolContext, handle: &str, text: &str) {
    let params = ReplaceBodyParams {
        path: "/a.md".to_string(),
        body: text.to_string(),
        changeset: handle.to_string(),
    };
    replace_body(ctx, params).await.expect("write a.md");
}

async fn publish_as(ctx: &ToolContext, handle: &str) -> Result<Published, OkfError> {
    let params = PublishParams {
        message: "race".to_string(),
        changeset: handle.to_string(),
    };
    publish(ctx, params).await
}

/// Deletes `name` with the CLI's actor, in its own task.
fn delete_in_task(
    test: &TestApp,
    name: &str,
) -> tokio::task::JoinHandle<Result<DeletedUser, OkfError>> {
    let pool = test.app.pool().clone();
    let name = name.to_string();
    tokio::spawn(async move {
        users::delete(
            &pool,
            &name,
            users::DeleteOptions::default(),
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
    })
}

/// Waits until `count` backends of this database wait on a lock.
async fn lock_waiters(test: &TestApp, count: i64) {
    let waited = tokio::time::timeout(LOCK_WAIT, async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE datname = current_database() AND wait_event_type = 'Lock'",
            )
            .fetch_one(test.app.pool())
            .await
            .expect("read pg_stat_activity");
            if waiting >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "{count} lock waiter(s) never appeared");
}

/// Awaits a spawned task under [`LOCK_WAIT`], naming `what` if it hangs.
async fn finish<T>(task: tokio::task::JoinHandle<T>, what: &str) -> T {
    tokio::time::timeout(LOCK_WAIT, task)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish: a lock cycle Postgres cannot see?"))
        .expect("the task panicked")
}

async fn state_of(test: &TestApp, handle: &str) -> ChangesetState {
    changeset_row(test, handle).await.state
}

#[tokio::test(flavor = "multi_thread")]
async fn a_publish_holding_its_changeset_finishes_before_its_users_deletion() {
    // Arrange: alice's changeset, and a holder of the catalog row, so the
    // publish stops at its generation bump, after its changeset lock.
    let test = pg().await;
    let alice = Arc::new(user_context(&test, "alice", Access::Write, false).await);
    let handle = open(&alice).await;
    body(&alice, &handle, "# A\n\nAlice.\n").await;
    let catalog_id = test
        .app
        .catalog("pg")
        .await
        .unwrap()
        .expect("catalog pg")
        .id();
    let mut holder = test.app.pool().begin().await.expect("begin the holder");
    sqlx::query("SELECT id FROM catalogs WHERE id = $1 FOR NO KEY UPDATE")
        .bind(catalog_id)
        .execute(&mut *holder)
        .await
        .expect("hold the catalog row");
    let publisher = Arc::clone(&alice);
    let to_publish = handle.clone();
    let publishing = tokio::spawn(async move { publish_as(&publisher, &to_publish).await });
    lock_waiters(&test, 1).await;
    let deleting = delete_in_task(&test, "alice");
    lock_waiters(&test, 2).await;

    // Act
    holder.rollback().await.expect("release the catalog row");
    let published = finish(publishing, "the publish").await;
    let deleted = finish(deleting, "the deletion").await;

    // Assert: the publish held alice before its changeset, so the deletion
    // waited for it and found nothing open; neither side is `Busy`.
    assert!(published.is_ok(), "{published:?}");
    assert_eq!(deleted.expect("the deletion").discarded, 0);
    assert_eq!(state_of(&test, &handle).await, ChangesetState::Published);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admins_discard_holding_a_changeset_finishes_before_its_owners_deletion() {
    // Arrange: an admin user's key discards alice's changeset, and a holder
    // of its entry stops the discard after its changeset lock. The deletion
    // takes every admin row first, the discard's actor included.
    let test = pg().await;
    let alice = user_context(&test, "alice", Access::Write, false).await;
    let boss = user_context(&test, "boss", Access::Admin, true).await;
    let handle = open(&alice).await;
    body(&alice, &handle, "# A\n\nAlice.\n").await;
    let changeset_id = changeset_row(&test, &handle).await.id;
    let mut holder = test.app.pool().begin().await.expect("begin the holder");
    sqlx::query("SELECT path FROM changeset_entries WHERE changeset_id = $1 FOR UPDATE")
        .bind(changeset_id)
        .execute(&mut *holder)
        .await
        .expect("hold the entry");
    let to_discard = handle.clone();
    let discarding = tokio::spawn(async move {
        let params = DiscardParams {
            paths: None,
            changeset: to_discard,
        };
        discard(&boss, params).await
    });
    lock_waiters(&test, 1).await;
    let deleting = delete_in_task(&test, "alice");
    lock_waiters(&test, 2).await;

    // Act
    holder.rollback().await.expect("release the entry");
    let discarded = finish(discarding, "the discard").await;
    let deleted = finish(deleting, "the deletion").await;

    // Assert
    assert!(discarded.expect("the discard").closed);
    assert_eq!(deleted.expect("the deletion").discarded, 0);
    assert_eq!(state_of(&test, &handle).await, ChangesetState::Discarded);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revert_holding_its_changeset_finishes_before_its_users_deletion() {
    // Arrange: alice reverts a.md in her changeset (an audited edit), and a
    // holder of the entry stops the revert after its changeset lock.
    let test = pg().await;
    let alice = Arc::new(user_context(&test, "alice", Access::Write, false).await);
    let handle = open(&alice).await;
    body(&alice, &handle, "# A\n\nAlice.\n").await;
    let changeset_id = changeset_row(&test, &handle).await.id;
    let mut holder = test.app.pool().begin().await.expect("begin the holder");
    sqlx::query("SELECT path FROM changeset_entries WHERE changeset_id = $1 FOR UPDATE")
        .bind(changeset_id)
        .execute(&mut *holder)
        .await
        .expect("hold the entry");
    let reverter = Arc::clone(&alice);
    let to_revert = handle.clone();
    let reverting = tokio::spawn(async move {
        let params = RevertParams {
            path: "/a.md".to_string(),
            revision: 1,
            changeset: to_revert,
        };
        revert(&reverter, params).await
    });
    lock_waiters(&test, 1).await;
    let deleting = delete_in_task(&test, "alice");
    lock_waiters(&test, 2).await;

    // Act
    holder.rollback().await.expect("release the entry");
    let reverted = finish(reverting, "the revert").await;
    let deleted = finish(deleting, "the deletion").await;

    // Assert: the revert committed first; the deletion then discarded the
    // changeset it left open.
    assert!(reverted.is_ok(), "{reverted:?}");
    assert_eq!(deleted.expect("the deletion").discarded, 1);
    assert_eq!(state_of(&test, &handle).await, ChangesetState::Discarded);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_publish_racing_its_users_deletion_never_deadlocks() {
    // The issue's race, unforced and repeated: one side wins cleanly every
    // time, and the losing publish is refused, never `Busy`.
    let test = pg().await;
    for round in 0..20 {
        // Arrange
        let name = format!("racer{round}");
        let racer = Arc::new(user_context(&test, &name, Access::Write, false).await);
        let handle = open(&racer).await;
        body(&racer, &handle, &format!("# A\n\nRound {round}.\n")).await;
        let publisher = Arc::clone(&racer);
        let to_publish = handle.clone();

        // Act: both start at once.
        let publishing = tokio::spawn(async move { publish_as(&publisher, &to_publish).await });
        let deleting = delete_in_task(&test, &name);
        let published = finish(publishing, "the publish").await;
        let deleted = finish(deleting, "the deletion").await;

        // Assert
        let deleted = deleted.unwrap_or_else(|error| panic!("round {round}: {error:?}"));
        let state = state_of(&test, &handle).await;
        match published {
            Ok(_) => {
                assert_eq!(deleted.discarded, 0, "round {round}");
                assert_eq!(state, ChangesetState::Published, "round {round}");
            }
            // The deletion won. A publish that resolved the handle first and
            // then waited finds the changeset closed under its lock
            // (`Refused`, "no longer open"); one that resolved it after the
            // commit finds its key gone, so the handle names nothing it may
            // use (`UnknownChangeset`). Never `Busy`.
            Err(error) => {
                let refused_under_the_lock = matches!(
                    &error,
                    OkfError::Refused { reason } if reason.contains("is no longer open")
                );
                assert!(
                    refused_under_the_lock || matches!(error, OkfError::UnknownChangeset { .. }),
                    "round {round}: {error:?}"
                );
                assert_eq!(deleted.discarded, 1, "round {round}");
                assert_eq!(state, ChangesetState::Discarded, "round {round}");
            }
        }
    }
}

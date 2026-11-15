use axum::http::StatusCode;
use chrono::Utc;
use serde_json::{json, Value};

use crate::db::{TEST_TEAM, TEST_USER};
use crate::http::api::testing::{app, every_catalog_admin, keys, send, Browser, Payload};
use crate::mcp::testing::TestApp;
use crate::store::audit::AuditActor;
use crate::store::keys::{self, Access, Grants, NewKey, NewProvisioner};
use crate::store::users::{self, AdminChange, NewUser};

/// Makes `user` an admin user (creating it if missing) with an admin key
/// `name` reaching `grants`, polled in.
///
/// # Returns
/// The key's secret.
async fn admin_user_key(test: &TestApp, user: &str, name: &str, grants: Grants<'_>) -> String {
    let pool = test.app.pool();
    if users::find(pool, user).await.is_err() {
        let new = NewUser {
            name: user,
            display_name: "",
        };
        users::create(pool, &new, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
    }
    let on = AdminChange {
        on: true,
        allow_last: true,
    };
    users::set_admin(pool, user, on, &AuditActor::cli(), Utc::now())
        .await
        .unwrap();
    let new = NewKey {
        name,
        access: Access::Admin,
        maintenance_report: false,
        grants,
        default_catalog: Some("main"),
        actor: name,
        user,
        self_issued: false,
        expires_at: None,
        levels: &[],
    };
    let key = keys::create_with_id(pool, &new, &AuditActor::cli(), Utc::now())
        .await
        .unwrap()
        .key;
    test.app.poll().await.unwrap();
    key
}

/// Admin user `staff` with an every-catalog admin key `staff-root`.
async fn staff_root(test: &TestApp) -> String {
    admin_user_key(test, "staff", "staff-root", Grants::All).await
}

/// A support unlock of `main` for one hour, with `extra` merged in.
fn body(extra: Value) -> Value {
    let mut body = json!({
        "catalog": "main", "category": "support", "reason": "ticket 12", "hours": 1
    });
    // `as_object_mut` borrows the JSON object as a map to extend in place.
    if let (Some(map), Value::Object(more)) = (body.as_object_mut(), extra) {
        map.extend(more);
    }
    body
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_unlocks_lists_and_ends_and_an_owner_sees_the_redacted_record() {
    let test = app().await;
    let staff = staff_root(&test).await;

    let (made, unlock) = send(
        &test,
        "POST",
        "/api/v1/unlocks",
        Some(&staff),
        Some(body(json!({}))),
    )
    .await;
    let id = unlock["id"].as_i64().unwrap();
    let (_, listed) = send(
        &test,
        "GET",
        "/api/v1/unlocks?catalog=main",
        Some(&staff),
        None,
    )
    .await;
    let mut owner = Browser::session_for(&test, TEST_USER).await;
    let owner_view = owner
        .send(
            &test,
            "GET",
            &format!("/api/v1/unlocks?team={TEST_TEAM}"),
            Payload::Empty,
        )
        .await;
    let (too_long, refusal) = send(
        &test,
        "POST",
        "/api/v1/unlocks",
        Some(&staff),
        Some(body(json!({"reason": "x", "hours": 9}))),
    )
    .await;
    let (ended, after) = send(
        &test,
        "DELETE",
        &format!("/api/v1/unlocks/{id}"),
        Some(&staff),
        None,
    )
    .await;
    let (again, _) = send(
        &test,
        "DELETE",
        &format!("/api/v1/unlocks/{id}"),
        Some(&staff),
        None,
    )
    .await;

    assert_eq!(made, StatusCode::CREATED, "{unlock}");
    assert_eq!(unlock["user"], "staff");
    assert_eq!(unlock["reason"], "ticket 12");
    assert_eq!(unlock["state"], "live");
    assert_eq!(unlock["owner_team"], TEST_TEAM);
    assert_eq!(unlock.get("ended"), Some(&Value::Null));
    assert_eq!(listed["unlocks"][0]["id"], id);
    let (status, seen_body) = owner_view.parts();
    assert_eq!(status, StatusCode::OK, "{seen_body}");
    let seen = &seen_body["unlocks"][0];
    assert_eq!(seen["id"], id);
    assert_eq!(seen["category"], "support");
    assert_eq!(seen["by"], "an operator");
    assert_eq!(seen["state"], "live");
    assert_eq!(seen["ends"], unlock["expires"]);
    assert!(
        seen.get("reason").is_none() && seen.get("user").is_none(),
        "{seen}"
    );
    let seen_text = seen_body.to_string();
    assert!(
        !seen_text.contains("staff") && !seen_text.contains("ticket 12"),
        "{seen_text}"
    );
    assert_eq!(too_long, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refusal["error"], "LimitExceeded");
    assert_eq!(ended, StatusCode::OK, "{after}");
    assert_eq!(after["state"], "ended");
    assert_eq!(again, StatusCode::CONFLICT);

    // Corrected (PR #375 review, 2026-11-13): the owner's whole audit
    // view, its export and its filters name no staff member.
    let team = format!("/api/v1/audit?team={TEST_TEAM}");
    let log = owner.send(&test, "GET", &team, Payload::Empty).await;
    let export = owner
        .send(
            &test,
            "GET",
            &format!("/api/v1/audit/export?team={TEST_TEAM}"),
            Payload::Empty,
        )
        .await;
    let probe = owner
        .send(
            &test,
            "GET",
            &format!("{team}&target=staff"),
            Payload::Empty,
        )
        .await;
    let probe_actor = owner
        .send(
            &test,
            "GET",
            &format!("{team}&actor=staff-root"),
            Payload::Empty,
        )
        .await;
    let (_, log_body) = log.parts();
    let log_text = log_body.to_string();
    assert!(log_text.contains("catalog.unlocked"), "{log_text}");
    assert!(log_text.contains("catalog.unlock_ended"), "{log_text}");
    for text in [log_text, export.text()] {
        assert!(
            !text.contains("staff") && !text.contains("ticket 12"),
            "{text}"
        );
    }
    assert_eq!(probe.parts().1["entries"], json!([]));
    assert_eq!(probe_actor.parts().1["entries"], json!([]));
}

/// How many unlocks exist, and how many `catalog.unlocked` entries.
async fn unlocks_and_entries(test: &TestApp) -> (i64, i64) {
    let unlocks: i64 = sqlx::query_scalar("SELECT count(*) FROM unlocks")
        .fetch_one(test.app.pool())
        .await
        .unwrap();
    let entries: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_entries WHERE action = 'catalog.unlocked'")
            .fetch_one(test.app.pool())
            .await
            .unwrap();
    (unlocks, entries)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hours_default_to_the_limit_and_an_admin_user_may_name_another_admin() {
    let test = app().await;
    let staff = staff_root(&test).await;
    let ops = admin_user_key(&test, "ops", "ops-root", Grants::All).await;

    let (status, unlock) = send(
        &test,
        "POST",
        "/api/v1/unlocks",
        Some(&ops),
        Some(json!({
            "catalog": "main", "category": "legal", "reason": "case 7", "user": "staff"
        })),
    )
    .await;
    let (_, live) = send(
        &test,
        "GET",
        "/api/v1/unlocks?live=true",
        Some(&staff),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{unlock}");
    assert_eq!(unlock["user"], "staff");
    assert_eq!(unlock["category"], "legal");
    let starts: chrono::DateTime<Utc> = unlock["starts"].as_str().unwrap().parse().unwrap();
    let expires: chrono::DateTime<Utc> = unlock["expires"].as_str().unwrap().parse().unwrap();
    // `unlock_max_hours` is 4 with no override.
    assert_eq!(expires - starts, chrono::TimeDelta::hours(4));
    assert_eq!(live["unlocks"].as_array().map(Vec::len), Some(1), "{live}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_admin_users_every_catalog_admin_key_cannot_unlock_and_writes_nothing() {
    // Issue #447: only operator staff make unlocks. `root` is an
    // every-catalog admin key of TEST_USER, who is not an admin user.
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    staff_root(&test).await;

    let mut answers = Vec::new();
    // Naming an admin target, naming nobody (its own user), and naming an
    // unknown user: all 403 before any target is looked at.
    for extra in [
        json!({"user": "staff"}),
        json!({}),
        json!({"user": "nobody"}),
    ] {
        answers.push(
            send(
                &test,
                "POST",
                "/api/v1/unlocks",
                Some(&root),
                Some(body(extra)),
            )
            .await,
        );
    }

    for (status, answer) in &answers {
        assert_eq!(*status, StatusCode::FORBIDDEN, "{answer}");
        assert!(
            answer["message"]
                .as_str()
                .is_some_and(|message| message.contains("not an admin user")),
            "{answer}"
        );
    }
    assert_eq!(unlocks_and_entries(&test).await, (0, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_admin_users_key_may_still_end_an_unlock_of_a_catalog_it_administers() {
    // Issue #447's ending rule: ending only takes access away, so it needs
    // an admin credential on the catalog, not an admin user.
    let test = app().await;
    let staff = staff_root(&test).await;
    let root = every_catalog_admin(&test).await;
    let (_, unlock) = send(
        &test,
        "POST",
        "/api/v1/unlocks",
        Some(&staff),
        Some(body(json!({}))),
    )
    .await;
    let id = unlock["id"].as_i64().unwrap();

    let (status, ended) = send(
        &test,
        "DELETE",
        &format!("/api/v1/unlocks/{id}"),
        Some(&root),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{ended}");
    assert_eq!(ended["state"], "ended");
}

#[tokio::test(flavor = "multi_thread")]
async fn unlocks_refuse_everyone_but_admins_and_scope_a_scoped_admin() {
    let test = app().await;
    let staff = staff_root(&test).await;
    let keys = keys(&test).await;
    let provisioner = keys::create_provisioner(
        test.app.pool(),
        &NewProvisioner {
            name: "signup",
            actor: "signup",
            user: TEST_USER,
        },
        &AuditActor::cli(),
        Utc::now(),
    )
    .await
    .unwrap()
    .key;
    test.app.poll().await.unwrap();
    let (_, unlock) = send(
        &test,
        "POST",
        "/api/v1/unlocks",
        Some(&staff),
        Some(body(json!({}))),
    )
    .await;
    let id = unlock["id"].as_i64().unwrap();
    let one = format!("/api/v1/unlocks/{id}");
    let only_main = ["main".to_string()];
    let staff_main =
        admin_user_key(&test, "staff", "staff-main", Grants::Catalogs(&only_main)).await;
    admin_user_key(&test, "ops", "ops-root", Grants::All).await;
    let by_team = format!("/api/v1/unlocks?team={TEST_TEAM}");

    let cases: Vec<(&str, &str, &str, Option<Value>, StatusCode)> = vec![
        // No credential.
        (
            "",
            "POST",
            "/api/v1/unlocks",
            Some(body(json!({}))),
            StatusCode::UNAUTHORIZED,
        ),
        // A provisioner is stopped by the gate, on every route.
        (
            &provisioner,
            "POST",
            "/api/v1/unlocks",
            Some(body(json!({}))),
            StatusCode::FORBIDDEN,
        ),
        (
            &provisioner,
            "GET",
            "/api/v1/unlocks",
            None,
            StatusCode::FORBIDDEN,
        ),
        (&provisioner, "DELETE", &one, None, StatusCode::FORBIDDEN),
        // Below admin, even naming the owner's team (a key, not a session).
        (
            &keys.write,
            "POST",
            "/api/v1/unlocks",
            Some(body(json!({}))),
            StatusCode::FORBIDDEN,
        ),
        (&keys.read, "GET", &by_team, None, StatusCode::FORBIDDEN),
        (&keys.read, "DELETE", &one, None, StatusCode::FORBIDDEN),
        // A scoped admin of `other` does not administer `main`: worded as
        // an unknown catalog or unlock.
        (
            &keys.stranger,
            "POST",
            "/api/v1/unlocks",
            Some(body(json!({}))),
            StatusCode::NOT_FOUND,
        ),
        (&keys.stranger, "DELETE", &one, None, StatusCode::NOT_FOUND),
        // A scoped admin of `main` may not name another user, even when
        // its user is an admin user.
        (
            &staff_main,
            "POST",
            "/api/v1/unlocks",
            Some(body(json!({"user": "ops"}))),
            StatusCode::FORBIDDEN,
        ),
        // Inputs.
        (
            &staff,
            "POST",
            "/api/v1/unlocks",
            Some(body(json!({"hours": 0}))),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            &staff,
            "POST",
            "/api/v1/unlocks",
            Some(body(json!({"reason": " "}))),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            &staff,
            "POST",
            "/api/v1/unlocks",
            Some(body(json!({"category": "audit"}))),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            &staff,
            "POST",
            "/api/v1/unlocks",
            Some(body(json!({"extra": 1}))),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            &staff,
            "POST",
            "/api/v1/unlocks",
            Some(body(json!({"catalog": "nope"}))),
            StatusCode::NOT_FOUND,
        ),
        (
            &staff,
            "POST",
            "/api/v1/unlocks",
            Some(body(json!({"user": "nobody"}))),
            StatusCode::NOT_FOUND,
        ),
        (
            &staff,
            "GET",
            "/api/v1/unlocks?colour=red",
            None,
            StatusCode::BAD_REQUEST,
        ),
        (
            &staff,
            "DELETE",
            "/api/v1/unlocks/999999",
            None,
            StatusCode::NOT_FOUND,
        ),
    ];
    for (key, method, uri, payload, want) in cases {
        let key = (!key.is_empty()).then_some(key);
        let (status, answer) = send(&test, method, uri, key, payload).await;
        assert_eq!(status, want, "{method} {uri}: {answer}");
    }

    // A scoped admin lists only the unlocks of the catalogs it administers.
    let (_, stranger_list) =
        send(&test, "GET", "/api/v1/unlocks", Some(&keys.stranger), None).await;
    let (_, admin_list) = send(&test, "GET", "/api/v1/unlocks", Some(&keys.admin), None).await;
    assert_eq!(stranger_list, json!({"unlocks": []}));
    assert_eq!(admin_list["unlocks"][0]["reason"], "ticket 12");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_an_owners_session_naming_its_team_reads_the_redacted_list() {
    let test = app().await;
    let staff = staff_root(&test).await;
    let root = every_catalog_admin(&test).await;
    // `main` moves to team `family`, owned by TEST_USER, with member `bob`.
    let steps = [
        ("POST", "/api/v1/users", json!({"name": "bob"})),
        (
            "POST",
            "/api/v1/teams",
            json!({"name": "family", "owner": TEST_USER}),
        ),
        (
            "PUT",
            "/api/v1/teams/family/members/bob",
            json!({"role": "member"}),
        ),
        (
            "PUT",
            "/api/v1/catalogs/main/owner",
            json!({"team": "family"}),
        ),
    ];
    for (method, uri, payload) in steps {
        let (status, answer) = send(&test, method, uri, Some(&root), Some(payload)).await;
        assert!(status.is_success(), "{method} {uri}: {answer}");
    }
    send(
        &test,
        "POST",
        "/api/v1/unlocks",
        Some(&staff),
        Some(body(json!({}))),
    )
    .await;
    let mut owner = Browser::session_for(&test, TEST_USER).await;
    let mut member = Browser::session_for(&test, "bob").await;
    let named = "/api/v1/unlocks?team=family";

    let no_team = owner
        .send(&test, "GET", "/api/v1/unlocks", Payload::Empty)
        .await;
    let malformed = owner
        .send(&test, "GET", "/api/v1/unlocks?team=a%00b", Payload::Empty)
        .await;
    let not_owned = owner
        .send(&test, "GET", "/api/v1/unlocks?team=bob", Payload::Empty)
        .await;
    let as_member = member.send(&test, "GET", named, Payload::Empty).await;
    let owned = owner.send(&test, "GET", named, Payload::Empty).await;
    // The catalog no longer belongs to the owner's personal team.
    let old_team = owner
        .send(
            &test,
            "GET",
            &format!("/api/v1/unlocks?team={TEST_TEAM}"),
            Payload::Empty,
        )
        .await;
    let other_catalog = owner
        .send(
            &test,
            "GET",
            &format!("{named}&catalog=other"),
            Payload::Empty,
        )
        .await;
    let staff_by_team = send(&test, "GET", named, Some(&staff), None).await;

    assert_eq!(no_team.parts().0, StatusCode::FORBIDDEN);
    assert_eq!(malformed.parts().0, StatusCode::FORBIDDEN);
    assert_eq!(not_owned.parts().0, StatusCode::FORBIDDEN);
    assert_eq!(as_member.parts().0, StatusCode::FORBIDDEN);
    let (status, seen) = owned.parts();
    assert_eq!(status, StatusCode::OK, "{seen}");
    assert_eq!(seen["unlocks"][0]["catalog"], "main");
    assert!(seen["unlocks"][0].get("reason").is_none(), "{seen}");
    assert_eq!(old_team.parts().1, json!({"unlocks": []}));
    assert_eq!(other_catalog.parts().1, json!({"unlocks": []}));
    // An admin naming the team still reads the full record.
    assert_eq!(staff_by_team.1["unlocks"][0]["reason"], "ticket 12");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_with_no_user_cannot_unlock_for_itself_or_anyone_else() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    // A pre-M12 key with no user; the generation bump makes the poll
    // reload the key cache.
    for statement in [
        "UPDATE api_keys SET user_id = NULL WHERE name = 'root'",
        "UPDATE server_state SET admin_generation = admin_generation + 1",
    ] {
        sqlx::query(statement)
            .execute(test.app.pool())
            .await
            .unwrap();
    }
    test.app.poll().await.unwrap();

    // Admin user `staff` exists, so naming it is a valid target for any
    // caller with a user.
    staff_root(&test).await;

    let mut answers = Vec::new();
    for extra in [json!({}), json!({"user": "staff"})] {
        answers.push(
            send(
                &test,
                "POST",
                "/api/v1/unlocks",
                Some(&root),
                Some(body(extra)),
            )
            .await,
        );
    }

    for (status, answer) in &answers {
        assert_eq!(*status, StatusCode::FORBIDDEN, "{answer}");
        assert!(
            answer["message"]
                .as_str()
                .is_some_and(|message| message.contains("give it a user")),
            "{answer}"
        );
    }
    assert_eq!(unlocks_and_entries(&test).await, (0, 0));
}

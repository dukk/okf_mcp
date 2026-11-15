use axum::http::StatusCode;
use serde_json::{json, Value};

use crate::db::TEST_USER;
use crate::http::api::testing::{app, every_catalog_admin, keys, send, Browser, Payload};
use crate::mcp::testing::TestApp;

/// The JSON Lines of an export, parsed.
fn lines(text: &str) -> Vec<Value> {
    text.lines()
        .map(|line| serde_json::from_str(line).expect("one JSON record per line"))
        .collect()
}

/// A provisioner key `signup` of TEST_USER, polled in.
async fn provisioner(test: &TestApp) -> String {
    let created = crate::store::keys::create_provisioner(
        test.app.pool(),
        &crate::store::keys::NewProvisioner {
            name: "signup",
            actor: "signup",
            user: TEST_USER,
        },
        &crate::store::audit::AuditActor::cli(),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    test.app.poll().await.unwrap();
    created.key
}

/// Team `family` owned by TEST_USER with member `bob`, and team `other`
/// owned by `bob`, all made by `root`.
async fn family(test: &TestApp, root: &str) {
    let steps = [
        ("POST", "/api/v1/users", json!({"name": "bob"})),
        ("POST", "/api/v1/teams", json!({"name": "family", "owner": TEST_USER})),
        ("PUT", "/api/v1/teams/family/members/bob", json!({"role": "member"})),
        ("POST", "/api/v1/teams", json!({"name": "other", "owner": "bob"})),
    ];
    for (method, uri, body) in steps {
        let (status, answer) = send(test, method, uri, Some(root), Some(body)).await;
        assert!(status.is_success(), "{method} {uri}: {status} {answer}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_reads_everything_newest_first_with_filters_and_a_cursor() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    for name in ["alice", "bob", "carol"] {
        send(&test, "POST", "/api/v1/users", Some(&root), Some(json!({"name": name}))).await;
    }

    let (status, page) = send(
        &test,
        "GET",
        "/api/v1/audit?action=user.created&limit=2",
        Some(&root),
        None,
    )
    .await;
    let after = page["next_after"].as_i64().expect("a full page has a cursor");
    let (_, rest) = send(
        &test,
        "GET",
        &format!("/api/v1/audit?action=user.created&limit=2&after={after}"),
        Some(&root),
        None,
    )
    .await;
    let (bad, refusal) = send(&test, "GET", "/api/v1/audit?action=no.such", Some(&root), None).await;
    let (bad_time, _) = send(&test, "GET", "/api/v1/audit?since=yesterday", Some(&root), None).await;
    let (unknown, _) = send(&test, "GET", "/api/v1/audit?colour=red", Some(&root), None).await;
    let (by_actor, mine) = send(&test, "GET", "/api/v1/audit?actor=root", Some(&root), None).await;

    assert_eq!(status, StatusCode::OK, "{page}");
    let names: Vec<&str> = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["target"]["person"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["carol", "bob"]);
    // The fixture user's own `user.created` is the oldest (issue #396).
    let rest_names: Vec<&str> = rest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["target"]["person"].as_str().unwrap())
        .collect();
    assert_eq!(rest_names, ["alice", TEST_USER]);
    assert_eq!(page["entries"][0]["actor"]["kind"], "known");
    assert_eq!(page["entries"][0]["actor"]["credential"].as_str().is_some(), true);
    assert_eq!(page["entries"][0]["actor"]["operator"], true);
    assert!(page["actions"].as_array().unwrap().contains(&json!("catalog.unlocked")));
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    assert_eq!(refusal["error"], "InvalidPath");
    assert_eq!(bad_time, StatusCode::BAD_REQUEST);
    assert_eq!(unknown, StatusCode::BAD_REQUEST, "unknown filters are refused");
    assert_eq!(by_actor, StatusCode::OK);
    assert_eq!(mine["entries"].as_array().unwrap().len(), 3, "{mine}");
    assert_eq!(mine.get("next_after"), Some(&Value::Null));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owner_session_sees_its_team_with_operators_redacted_and_a_member_sees_nothing() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    family(&test, &root).await;
    let mut owner = Browser::session_for(&test, TEST_USER).await;
    let mut member = Browser::session_for(&test, "bob").await;

    let mine = owner.send(&test, "GET", "/api/v1/audit?team=family", Payload::Empty).await;
    let not_mine = owner.send(&test, "GET", "/api/v1/audit?team=other", Payload::Empty).await;
    let no_team = owner.send(&test, "GET", "/api/v1/audit", Payload::Empty).await;
    let malformed = owner.send(&test, "GET", "/api/v1/audit?team=a%00b", Payload::Empty).await;
    let as_member = member.send(&test, "GET", "/api/v1/audit?team=family", Payload::Empty).await;
    let bobs_own = member.send(&test, "GET", "/api/v1/audit?team=other", Payload::Empty).await;
    let scoped = keys(&test).await;
    let (by_key, _) = send(&test, "GET", "/api/v1/audit?team=family", Some(&scoped.admin), None).await;
    let (admin_malformed, admin_body) =
        send(&test, "GET", "/api/v1/audit?team=a%00b", Some(&root), None).await;
    let export = owner
        .send(&test, "GET", "/api/v1/audit/export?team=family", Payload::Empty)
        .await;

    let (status, body) = mine.parts();
    assert_eq!(status, StatusCode::OK, "{body}");
    let actions: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["team.member_added", "team.created"], "only family's entries");
    assert_eq!(body["entries"][0]["actor"], json!({"kind": "operator"}));
    // `get`: `entry["ip"]` is `Null` for a missing key too (lessons.md).
    assert_eq!(body["entries"][0].get("ip"), Some(&Value::Null));
    assert_eq!(body["entries"][0]["teams"], json!(["family"]));
    assert_eq!(body["entries"][0]["target"]["person"], "bob");
    assert!(!body.to_string().contains("root"), "{body}");
    assert_eq!(lines(&export.text()), body["entries"].as_array().unwrap().clone());
    assert_eq!(not_mine.status, StatusCode::FORBIDDEN);
    assert_eq!(no_team.status, StatusCode::FORBIDDEN, "an owner names its team");
    assert_eq!(malformed.status, StatusCode::FORBIDDEN);
    assert_eq!(as_member.status, StatusCode::FORBIDDEN);
    assert_eq!(bobs_own.status, StatusCode::OK, "bob owns other");
    assert_eq!(by_key, StatusCode::FORBIDDEN, "owners read through a console session only");
    assert_eq!(admin_malformed, StatusCode::OK);
    assert_eq!(admin_body["entries"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provisioner_reads_only_its_own_entries() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    let key = provisioner(&test).await;
    send(&test, "POST", "/api/v1/users", Some(&root), Some(json!({"name": "by-root"}))).await;
    send(&test, "POST", "/api/v1/users", Some(&key), Some(json!({"name": "by-signup"}))).await;

    let (status, page) = send(&test, "GET", "/api/v1/audit?action=user.created", Some(&key), None).await;
    let (all, everything) = send(&test, "GET", "/api/v1/audit", Some(&key), None).await;
    let (export, _) = send(&test, "GET", "/api/v1/audit/export", Some(&key), None).await;

    assert_eq!(status, StatusCode::OK, "{page}");
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["target"]["person"], "by-signup");
    assert_eq!(all, StatusCode::OK);
    // Its own creation was the CLI's, so its only entry is the user it made.
    assert_eq!(everything["entries"].as_array().unwrap().len(), 1, "{everything}");
    assert_eq!(export, StatusCode::FORBIDDEN, "the provisioner gate allows GET /audit only");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_export_is_json_lines_with_the_same_filters() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    send(&test, "POST", "/api/v1/users", Some(&root), Some(json!({"name": "alice"}))).await;
    let request = axum::http::Request::builder()
        .uri("/api/v1/audit/export?action=user.created")
        .header("authorization", format!("Bearer {root}"))
        .body(axum::body::Body::empty())
        .unwrap();

    // `oneshot` sends one request through the whole router (auth included).
    let response = tower::ServiceExt::oneshot(
        crate::http::router(
            std::sync::Arc::clone(&test.app),
            &tokio_util::sync::CancellationToken::new(),
        ),
        request,
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/x-ndjson");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let records = lines(std::str::from_utf8(&bytes).unwrap());
    // The fixture user's own `user.created` entry is the older one
    // (corrected, issue #396, 2026-11-14: `test_db` records it).
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["action"], "user.created");
    assert_eq!(records[0]["target"]["person"], "alice");
    assert_eq!(records[1]["target"]["person"], TEST_USER);
}

#[tokio::test(flavor = "multi_thread")]
async fn me_lists_the_teams_the_user_owns() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    family(&test, &root).await;
    let mut owner = Browser::session_for(&test, TEST_USER).await;
    let mut member = Browser::session_for(&test, "bob").await;

    let (_, mine) = owner.send(&test, "GET", "/api/v1/me", Payload::Empty).await.parts();
    let (_, bobs) = member.send(&test, "GET", "/api/v1/me", Payload::Empty).await.parts();

    assert_eq!(mine["owned_teams"], json!(["family", TEST_USER]));
    assert_eq!(bobs["owned_teams"], json!(["bob", "other"]), "not family, where bob is a member");
}

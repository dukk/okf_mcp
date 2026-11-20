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

/// A provisioner key `signup` of TEST_USER.
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
    created.key
}

/// Team `family` owned by TEST_USER with member `bob`, and team `other`
/// owned by `bob`, all made by `root`.
async fn family(test: &TestApp, root: &str) {
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
            "POST",
            "/api/v1/teams",
            json!({"name": "other", "owner": "bob"}),
        ),
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
        send(
            &test,
            "POST",
            "/api/v1/users",
            Some(&root),
            Some(json!({"name": name})),
        )
        .await;
    }

    let (status, page) = send(
        &test,
        "GET",
        "/api/v1/audit?action=user.created&limit=2",
        Some(&root),
        None,
    )
    .await;
    let after = page["next_after"]
        .as_i64()
        .expect("a full page has a cursor");
    let (_, rest) = send(
        &test,
        "GET",
        &format!("/api/v1/audit?action=user.created&limit=2&after={after}"),
        Some(&root),
        None,
    )
    .await;
    let (bad, refusal) = send(
        &test,
        "GET",
        "/api/v1/audit?action=no.such",
        Some(&root),
        None,
    )
    .await;
    let (bad_time, _) = send(
        &test,
        "GET",
        "/api/v1/audit?since=yesterday",
        Some(&root),
        None,
    )
    .await;
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
    assert!(page["entries"][0]["actor"]["credential"].is_string());
    assert_eq!(page["entries"][0]["actor"]["operator"], true);
    assert!(page["actions"]
        .as_array()
        .unwrap()
        .contains(&json!("catalog.unlocked")));
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    assert_eq!(refusal["error"], "InvalidPath");
    assert_eq!(bad_time, StatusCode::BAD_REQUEST);
    assert_eq!(
        unknown,
        StatusCode::BAD_REQUEST,
        "unknown filters are refused"
    );
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

    let mine = owner
        .send(&test, "GET", "/api/v1/audit?team=family", Payload::Empty)
        .await;
    let not_mine = owner
        .send(&test, "GET", "/api/v1/audit?team=other", Payload::Empty)
        .await;
    let no_team = owner
        .send(&test, "GET", "/api/v1/audit", Payload::Empty)
        .await;
    let malformed = owner
        .send(&test, "GET", "/api/v1/audit?team=a%00b", Payload::Empty)
        .await;
    let as_member = member
        .send(&test, "GET", "/api/v1/audit?team=family", Payload::Empty)
        .await;
    let bobs_own = member
        .send(&test, "GET", "/api/v1/audit?team=other", Payload::Empty)
        .await;
    let scoped = keys(&test).await;
    let (by_key, _) = send(
        &test,
        "GET",
        "/api/v1/audit?team=family",
        Some(&scoped.admin),
        None,
    )
    .await;
    let (admin_malformed, admin_body) =
        send(&test, "GET", "/api/v1/audit?team=a%00b", Some(&root), None).await;
    let export = owner
        .send(
            &test,
            "GET",
            "/api/v1/audit/export?team=family",
            Payload::Empty,
        )
        .await;

    let (status, body) = mine.parts();
    assert_eq!(status, StatusCode::OK, "{body}");
    let actions: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions,
        ["team.member_added", "team.created"],
        "only family's entries"
    );
    assert_eq!(body["entries"][0]["actor"], json!({"kind": "operator"}));
    // `get`: `entry["ip"]` is `Null` for a missing key too (lessons.md).
    assert_eq!(body["entries"][0].get("ip"), Some(&Value::Null));
    assert_eq!(body["entries"][0]["teams"], json!(["family"]));
    assert_eq!(body["entries"][0]["target"]["person"], "bob");
    assert!(!body.to_string().contains("root"), "{body}");
    assert_eq!(
        lines(&export.text()),
        body["entries"].as_array().unwrap().clone()
    );
    assert_eq!(not_mine.status, StatusCode::FORBIDDEN);
    assert_eq!(
        no_team.status,
        StatusCode::FORBIDDEN,
        "an owner names its team"
    );
    assert_eq!(malformed.status, StatusCode::FORBIDDEN);
    assert_eq!(as_member.status, StatusCode::FORBIDDEN);
    assert_eq!(bobs_own.status, StatusCode::OK, "bob owns other");
    assert_eq!(
        by_key,
        StatusCode::FORBIDDEN,
        "owners read through a console session only"
    );
    assert_eq!(admin_malformed, StatusCode::OK);
    assert_eq!(admin_body["entries"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provisioner_reads_only_its_own_entries() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    let key = provisioner(&test).await;
    send(
        &test,
        "POST",
        "/api/v1/users",
        Some(&root),
        Some(json!({"name": "by-root"})),
    )
    .await;
    send(
        &test,
        "POST",
        "/api/v1/users",
        Some(&key),
        Some(json!({"name": "by-signup"})),
    )
    .await;

    let (status, page) = send(
        &test,
        "GET",
        "/api/v1/audit?action=user.created",
        Some(&key),
        None,
    )
    .await;
    let (all, everything) = send(&test, "GET", "/api/v1/audit", Some(&key), None).await;
    let (export, _) = send(&test, "GET", "/api/v1/audit/export", Some(&key), None).await;

    assert_eq!(status, StatusCode::OK, "{page}");
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["target"]["person"], "by-signup");
    assert_eq!(all, StatusCode::OK);
    // Its own creation was the CLI's, so its only entry is the user it made.
    assert_eq!(
        everything["entries"].as_array().unwrap().len(),
        1,
        "{everything}"
    );
    assert_eq!(
        export,
        StatusCode::FORBIDDEN,
        "the provisioner gate allows GET /audit only"
    );
}

/// Exports as `key` and answers the parsed lines' ids, waiting at most
/// 20 s (an export whose cursor never advances would loop forever).
async fn export_ids(test: &TestApp, key: &str, query: &str) -> Vec<i64> {
    let mut browser = Browser::default();
    browser.bearer = Some(key.to_string());
    let uri = format!("/api/v1/audit/export?{query}");
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        browser.send(test, "GET", &uri, Payload::Empty),
    )
    .await
    .expect("the export finished within 20 s");
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    lines(&reply.text())
        .iter()
        .map(|record| record["id"].as_i64().expect("an entry id"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_export_pages_through_every_entry_once_newest_first_from_any_cursor() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    // 1203 matching entries in one statement: two full pages and a partial
    // one, so the export must advance its cursor twice.
    let mut expected: Vec<i64> = sqlx::query_scalar(
        "INSERT INTO audit_entries (at, action, source, actor_label, actor_operator, \
             target_kind, target_name) \
         SELECT now(), 'setting.changed', 'cli', 'cli:test', true, 'setting', 's' || n \
         FROM generate_series(1, 1203) AS n RETURNING id",
    )
    .fetch_all(test.app.pool())
    .await
    .unwrap();
    expected.sort_unstable_by(|a, b| b.cmp(a));
    let cursor = expected[600];

    // `limit` is ignored by the export: it pages to the end.
    let all = export_ids(&test, &root, "action=setting.changed&limit=3").await;
    let from_cursor = export_ids(
        &test,
        &root,
        &format!("action=setting.changed&limit=3&after={cursor}"),
    )
    .await;

    assert_eq!(all.len(), 1203);
    assert_eq!(all, expected, "every entry once, newest first");
    assert_eq!(
        from_cursor,
        expected[601..],
        "only the entries older than the cursor"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_export_is_json_lines_with_the_same_filters() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    send(
        &test,
        "POST",
        "/api/v1/users",
        Some(&root),
        Some(json!({"name": "alice"})),
    )
    .await;
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
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
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

    let (_, mine) = owner
        .send(&test, "GET", "/api/v1/me", Payload::Empty)
        .await
        .parts();
    let (_, bobs) = member
        .send(&test, "GET", "/api/v1/me", Payload::Empty)
        .await
        .parts();

    assert_eq!(mine["owned_teams"], json!(["family", TEST_USER]));
    assert_eq!(
        bobs["owned_teams"],
        json!(["bob", "other"]),
        "not family, where bob is a member"
    );
}

/// The `entries` of `GET /api/v1/audit?{query}` as `key`.
async fn entries(test: &TestApp, key: &str, query: &str) -> Vec<Value> {
    let (status, page) = send(
        test,
        "GET",
        &format!("/api/v1/audit?{query}"),
        Some(key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    page["entries"].as_array().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_person_shows_by_the_snapshot_label_and_a_deleted_user_by_the_placeholder() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    let (status, answer) = send(
        &test,
        "POST",
        "/api/v1/users",
        Some(&root),
        Some(json!({"name": "alice", "display_name": "Alice Example"})),
    )
    .await;
    assert!(status.is_success(), "{answer}");
    let (status, answer) = send(
        &test,
        "POST",
        "/api/v1/users",
        Some(&root),
        Some(json!({"name": "ghost"})),
    )
    .await;
    assert!(status.is_success(), "{answer}");

    let before = entries(&test, &root, "action=user.created").await;
    let ghost_before = &before[0]["target"];
    let alice = &before[1]["target"];
    let (status, answer) = send(&test, "DELETE", "/api/v1/users/alice", Some(&root), None).await;
    assert!(status.is_success(), "{answer}");
    let after = entries(&test, &root, "action=user.created").await;

    assert_eq!(alice["person"], "alice");
    assert_eq!(alice["person_label"], "Alice Example");
    // The acting key's user is a person too, labelled from its own snapshot.
    assert_eq!(before[1]["actor"]["person"], TEST_USER);
    assert!(before[1]["actor"]["person_label"].is_string());
    assert_eq!(
        ghost_before["person_label"], "Unnamed user",
        "no display name, no email"
    );
    let forgotten = after
        .iter()
        .find(|entry| {
            entry["target"]["person"]
                .as_str()
                .unwrap()
                .starts_with("deleted user")
        })
        .expect("alice's entry names a deleted user");
    assert_eq!(forgotten["target"]["person_label"], "Unnamed user");
    assert!(
        !forgotten.to_string().contains("Alice Example"),
        "deletion blanks the snapshot: {forgotten}"
    );
    // The filters still match the slug a live person has.
    assert_eq!(entries(&test, &root, "target=ghost").await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_catalog_and_a_shared_team_target_show_their_label_while_they_exist() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    let steps = [
        (
            "POST",
            "/api/v1/teams",
            json!({"name": "family", "display_name": "The Family", "owner": TEST_USER}),
        ),
        (
            "POST",
            "/api/v1/catalogs",
            json!({"name": "work", "display_name": "Work notes", "owner": "family"}),
        ),
    ];
    for (method, uri, body) in steps {
        let (status, answer) = send(&test, method, uri, Some(&root), Some(body)).await;
        assert!(status.is_success(), "{method} {uri}: {status} {answer}");
    }

    let catalog = entries(&test, &root, "action=catalog.created").await;
    let team = entries(&test, &root, "action=team.created").await;
    let (status, answer) = send(&test, "DELETE", "/api/v1/catalogs/work", Some(&root), None).await;
    assert!(status.is_success(), "{answer}");
    let deleted = entries(&test, &root, "action=catalog.created").await;

    assert_eq!(catalog[0]["target"]["target_label"], "Work notes");
    assert_eq!(
        catalog[0]["target"]["name"], "work",
        "the slug stays for filters"
    );
    let family = team
        .iter()
        .find(|entry| entry["target"]["name"] == "family")
        .expect("family's team.created");
    assert_eq!(family["target"]["target_label"], "The Family");
    assert_eq!(
        deleted[0]["target"].get("target_label"),
        Some(&Value::Null),
        "a deleted catalog has no label"
    );
    assert_eq!(
        entries(&test, &root, "target=work").await.len(),
        2,
        "the slug still filters"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_export_carries_the_labels() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    send(
        &test,
        "POST",
        "/api/v1/users",
        Some(&root),
        Some(json!({"name": "alice", "display_name": "Alice Example"})),
    )
    .await;
    let request = axum::http::Request::builder()
        .uri("/api/v1/audit/export?action=user.created&target=alice")
        .header("authorization", format!("Bearer {root}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let response = tower::ServiceExt::oneshot(
        crate::http::router(
            std::sync::Arc::clone(&test.app),
            &tokio_util::sync::CancellationToken::new(),
        ),
        request,
    )
    .await
    .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let records = lines(std::str::from_utf8(&bytes).unwrap());

    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["target"]["person"], "alice");
    assert_eq!(records[0]["target"]["person_label"], "Alice Example");
}

/// The audit export's JSON Lines for `query`, as `key`.
async fn export(test: &TestApp, key: &str, query: &str) -> Vec<Value> {
    let request = axum::http::Request::builder()
        .uri(format!("/api/v1/audit/export?{query}"))
        .header("authorization", format!("Bearer {key}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let response = tower::ServiceExt::oneshot(
        crate::http::router(
            std::sync::Arc::clone(&test.app),
            &tokio_util::sync::CancellationToken::new(),
        ),
        request,
    )
    .await
    .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    lines(std::str::from_utf8(&bytes).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_team_or_catalog_recreated_under_an_old_name_never_lends_its_label_to_the_old_entries() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    let team = |label: &str| json!({"name": "family", "display_name": label, "owner": TEST_USER});
    let catalog = |label: &str| json!({"name": "work", "display_name": label, "owner": "family"});
    let steps = [
        ("POST", "/api/v1/teams", Some(team("Old family"))),
        ("POST", "/api/v1/catalogs", Some(catalog("Old work"))),
        ("DELETE", "/api/v1/catalogs/work", None),
        ("DELETE", "/api/v1/teams/family", None),
        ("POST", "/api/v1/teams", Some(team("New family"))),
        ("POST", "/api/v1/catalogs", Some(catalog("New work"))),
    ];
    for (method, uri, body) in steps {
        let (status, answer) = send(&test, method, uri, Some(&root), body).await;
        assert!(status.is_success(), "{method} {uri}: {status} {answer}");
    }

    let listed = entries(&test, &root, "limit=200").await;
    let exported = export(&test, &root, "").await;

    for records in [listed, exported] {
        // Oldest first: the old team's and catalog's entries, then the new.
        let labels = |kind: &str, action: &str| -> Vec<Value> {
            let name = if kind == "team" { "family" } else { "work" };
            records
                .iter()
                .rev()
                .filter(|entry| {
                    entry["target"]["kind"] == kind
                        && entry["target"]["name"] == name
                        && entry["action"] == action
                })
                .map(|entry| entry["target"].get("target_label").cloned().unwrap())
                .collect()
        };
        assert_eq!(
            labels("team", "team.created"),
            [Value::Null, json!("New family")]
        );
        assert_eq!(labels("team", "team.deleted"), [Value::Null]);
        assert_eq!(
            labels("catalog", "catalog.created"),
            [Value::Null, json!("New work")]
        );
        assert_eq!(labels("catalog", "catalog.deleted"), [Value::Null]);
    }
}

/// Teams `family` (owned by TEST_USER) and `other` (owned by `bob`), as
/// [`family`] makes them, then `rounds` members added to each in turn, so
/// other teams' entries sit between every two of family's. Entry ids start
/// above 7,000,000 first, so no id can pass for a small number elsewhere
/// in a payload.
async fn interleaved(test: &TestApp, root: &str, rounds: usize) {
    sqlx::query("SELECT setval(pg_get_serial_sequence('audit_entries', 'id'), 7000000)")
        .execute(test.app.pool())
        .await
        .unwrap();
    family(test, root).await;
    for round in 0..rounds {
        for (team, user) in [
            ("family", format!("fam-{round}")),
            ("other", format!("oth-{round}")),
        ] {
            let steps = [
                ("POST", "/api/v1/users".to_string(), json!({"name": user})),
                (
                    "PUT",
                    format!("/api/v1/teams/{team}/members/{user}"),
                    json!({"role": "member"}),
                ),
            ];
            for (method, uri, body) in steps {
                let (status, answer) = send(test, method, &uri, Some(root), Some(body)).await;
                assert!(status.is_success(), "{method} {uri}: {status} {answer}");
            }
        }
    }
}

/// Every entry id [`interleaved`] numbered (the fixture's own entries,
/// written before it, have small ids that could match any digit).
async fn every_id(test: &TestApp) -> Vec<i64> {
    sqlx::query_scalar("SELECT id FROM audit_entries WHERE id > 7000000 ORDER BY id")
        .fetch_all(test.app.pool())
        .await
        .unwrap()
}

/// An owner's `GET` of `uri`, answering the status and the JSON body.
async fn owner_get(test: &TestApp, owner: &mut Browser, uri: &str) -> (StatusCode, Value) {
    owner.send(test, "GET", uri, Payload::Empty).await.parts()
}

/// `token` with one character changed to another base64url character.
fn tampered(token: &str) -> String {
    let mut chars: Vec<char> = token.chars().collect();
    let middle = chars.len() / 2;
    chars[middle] = if chars[middle] == 'A' { 'B' } else { 'A' };
    chars.into_iter().collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owners_page_and_export_carry_no_global_entry_id() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    interleaved(&test, &root, 4).await;
    let mut owner = Browser::session_for(&test, TEST_USER).await;

    let (_, admin_view) = send(&test, "GET", "/api/v1/audit?team=family", Some(&root), None).await;
    let family_ids: Vec<i64> = admin_view["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_i64().expect("an admin sees ids"))
        .collect();
    let (status, page) = owner_get(&test, &mut owner, "/api/v1/audit?team=family&limit=2").await;
    let (_, again) = owner_get(&test, &mut owner, "/api/v1/audit?team=family&limit=2").await;
    let (_, whole) = owner_get(&test, &mut owner, "/api/v1/audit?team=family").await;
    let export = owner
        .send(
            &test,
            "GET",
            "/api/v1/audit/export?team=family",
            Payload::Empty,
        )
        .await;
    let ids = every_id(&test).await;

    assert!(ids.len() > 6, "{ids:?}");
    // The seed is what the test needs: family's ids have gaps, which is
    // what a raw id would reveal.
    assert_eq!(family_ids.len(), 6, "{admin_view}");
    // Newest first: the four members, then bob's addition, which came
    // straight after family's creation; another team's entry sits between
    // every other two.
    assert!(
        family_ids[..5].windows(2).all(|pair| pair[0] - pair[1] > 1),
        "other teams' entries between every two of family's: {family_ids:?}"
    );
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(whole["entries"].as_array().unwrap().len(), 6, "{whole}");
    let exported = lines(&export.text());
    assert_eq!(exported, whole["entries"].as_array().unwrap().clone());
    for entry in whole["entries"].as_array().unwrap().iter().chain(&exported) {
        assert_eq!(entry.get("id"), None, "no id key: {entry}");
    }
    let cursor = page["next_after"]
        .as_str()
        .expect("an owner's cursor is a sealed string");
    assert_ne!(
        Some(cursor),
        again["next_after"].as_str(),
        "two cursors for one place look unrelated"
    );
    // No entry id, of any team, appears anywhere in what the owner got.
    for text in [page.to_string(), whole.to_string(), export.text()] {
        for id in &ids {
            assert!(!text.contains(&id.to_string()), "entry id {id} in {text}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owner_pages_through_every_entry_once_with_sealed_cursors() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    interleaved(&test, &root, 3).await;
    let mut owner = Browser::session_for(&test, TEST_USER).await;

    let (_, whole) = owner_get(&test, &mut owner, "/api/v1/audit?team=family").await;
    let mut paged = Vec::new();
    let mut cursors = Vec::new();
    let mut uri = "/api/v1/audit?team=family&limit=2".to_string();
    // Bounded: a cursor that never advanced would loop forever.
    for _ in 0..20 {
        let (status, page) = owner_get(&test, &mut owner, &uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {page}");
        paged.extend(page["entries"].as_array().unwrap().iter().cloned());
        let Some(next) = page["next_after"].as_str() else {
            break;
        };
        cursors.push(next.to_string());
        uri = format!("/api/v1/audit?after={next}&team=family&limit=2");
    }
    let from_first = lines(
        &owner
            .send(
                &test,
                "GET",
                &format!("/api/v1/audit/export?after={}&team=family", cursors[0]),
                Payload::Empty,
            )
            .await
            .text(),
    );

    let whole = whole["entries"].as_array().unwrap().clone();
    assert_eq!(whole.len(), 5, "{whole:?}");
    assert_eq!(paged, whole, "every entry once, newest first");
    // Five entries at two a page: three pages, the last not full.
    assert_eq!(cursors.len(), 2);
    assert_eq!(
        from_first,
        whole[2..],
        "the export starts after an owner's cursor"
    );
    assert_eq!(from_first.len(), 3, "{from_first:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tampered_raw_or_foreign_cursor_is_refused_and_shows_nothing() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    interleaved(&test, &root, 2).await;
    // TEST_USER owns a second team with entries of its own.
    for (method, uri, body) in [
        (
            "POST",
            "/api/v1/teams",
            json!({"name": "second", "owner": TEST_USER}),
        ),
        (
            "PUT",
            "/api/v1/teams/second/members/oth-0",
            json!({"role": "member"}),
        ),
    ] {
        let (status, answer) = send(&test, method, uri, Some(&root), Some(body)).await;
        assert!(status.is_success(), "{method} {uri}: {status} {answer}");
    }
    let mut owner = Browser::session_for(&test, TEST_USER).await;
    let (_, page) = owner_get(&test, &mut owner, "/api/v1/audit?team=family&limit=1").await;
    let cursor = page["next_after"].as_str().unwrap().to_string();
    let raw = every_id(&test).await.last().copied().unwrap();

    let mut refusals = Vec::new();
    for path in ["/api/v1/audit", "/api/v1/audit/export"] {
        for (team, after) in [
            ("family", tampered(&cursor)),
            ("family", raw.to_string()),
            ("family", "not-a-cursor".to_string()),
            ("family", format!("{cursor}A")),
            ("second", cursor.clone()),
        ] {
            let uri = format!("{path}?team={team}&after={after}");
            let reply = owner.send(&test, "GET", &uri, Payload::Empty).await;
            refusals.push((uri, reply.status, reply.json(), reply.text()));
        }
    }
    let (good, rest) = owner_get(
        &test,
        &mut owner,
        &format!("/api/v1/audit?team=family&after={cursor}"),
    )
    .await;

    for (uri, status, body, text) in &refusals {
        assert_eq!(*status, StatusCode::BAD_REQUEST, "{uri}: {text}");
        assert_eq!(body["error"], "InvalidPath", "{uri}: {text}");
        assert_eq!(body.get("entries"), None, "{uri}: {text}");
    }
    // Every refusal reads the same, whatever was wrong with the cursor.
    let reasons: std::collections::BTreeSet<&str> = refusals
        .iter()
        .map(|(_, _, body, _)| body["message"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert_eq!(
        good,
        StatusCode::OK,
        "the genuine cursor still works: {rest}"
    );
    assert_eq!(rest["entries"].as_array().unwrap().len(), 3, "{rest}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rotated_key_refuses_the_cursors_it_sealed() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    interleaved(&test, &root, 1).await;
    let mut owner = Browser::session_for(&test, TEST_USER).await;
    let (_, page) = owner_get(&test, &mut owner, "/api/v1/audit?team=family&limit=1").await;
    let cursor = page["next_after"].as_str().unwrap().to_string();

    sqlx::query("DELETE FROM server_keys")
        .execute(test.app.pool())
        .await
        .unwrap();
    let (stale, _) = owner_get(
        &test,
        &mut owner,
        &format!("/api/v1/audit?team=family&after={cursor}"),
    )
    .await;
    let (fresh, first) = owner_get(&test, &mut owner, "/api/v1/audit?team=family&limit=1").await;
    let next = first["next_after"].as_str().unwrap().to_string();
    let (resumed, _) = owner_get(
        &test,
        &mut owner,
        &format!("/api/v1/audit?team=family&after={next}"),
    )
    .await;

    assert_eq!(stale, StatusCode::BAD_REQUEST);
    assert_eq!(fresh, StatusCode::OK);
    assert_eq!(resumed, StatusCode::OK, "a cursor under the new key opens");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_still_pages_by_raw_ids_and_refuses_an_owners_cursor() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    interleaved(&test, &root, 1).await;
    let mut owner = Browser::session_for(&test, TEST_USER).await;
    let (_, owned) = owner_get(&test, &mut owner, "/api/v1/audit?team=family&limit=1").await;
    let sealed = owned["next_after"].as_str().unwrap().to_string();

    let (status, page) = send(
        &test,
        "GET",
        "/api/v1/audit?team=family&limit=1",
        Some(&root),
        None,
    )
    .await;
    let first_id = page["entries"][0]["id"]
        .as_i64()
        .expect("an admin sees the id");
    let after = page["next_after"]
        .as_i64()
        .expect("an admin's cursor is the raw id");
    let (_, rest) = send(
        &test,
        "GET",
        &format!("/api/v1/audit?team=family&after={after}"),
        Some(&root),
        None,
    )
    .await;
    let (refused, body) = send(
        &test,
        "GET",
        &format!("/api/v1/audit?team=family&after={sealed}"),
        Some(&root),
        None,
    )
    .await;
    let export = export_ids(&test, &root, &format!("team=family&after={after}")).await;

    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(after, first_id, "the cursor is the last entry's id");
    let rest_ids: Vec<i64> = rest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_i64().unwrap())
        .collect();
    assert!(
        !rest_ids.is_empty() && rest_ids.iter().all(|id| *id < after),
        "{rest}"
    );
    assert_eq!(export, rest_ids, "the export starts after a raw id too");
    assert_eq!(refused, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "InvalidPath");
}

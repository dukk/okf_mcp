//! Route tests for display names (M17 Task 5): the labels on the views, the
//! scoped `GET /labels`, and the three edit routes with each refusal.

use axum::http::StatusCode;
use serde_json::{json, Value};

use crate::db::{TEST_TEAM, TEST_USER};
use crate::http::api::testing::{app, every_catalog_admin, keys, send, Browser, Payload};
use crate::labels::user_label;
use crate::mcp::testing::{suspend_catalogs, TestApp};
use crate::store::keys::Access;

/// POSTs `body` as the every-catalog admin `root` and returns the answer.
async fn post(test: &TestApp, root: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    send(test, "POST", uri, Some(root), Some(body)).await
}

/// PUTs `body` as `root`, polling happens in the handler.
async fn put(test: &TestApp, root: &str, uri: &str, body: Value) {
    let (status, answer) = send(test, "PUT", uri, Some(root), Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {answer}");
}

/// A session PATCH.
async fn patch(
    browser: &mut Browser,
    test: &TestApp,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let reply = browser.send(test, "PATCH", uri, Payload::Json(body)).await;
    (reply.status, reply.json())
}

/// Users `alice` ("Alice Example", owner of shared team `acme` named
/// "Acme") and `bob` (no display name, a member of `acme`), and catalog
/// `acme-notes` ("Acme notes") owned by `acme`, open to its members.
async fn acme(test: &TestApp, root: &str) {
    let users = [("alice", "Alice Example"), ("bob", "")];
    for (name, display_name) in users {
        let (status, answer) = post(
            test,
            root,
            "/api/v1/users",
            json!({"name": name, "display_name": display_name}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
    }
    let (status, answer) = post(
        test,
        root,
        "/api/v1/teams",
        json!({"name": "acme", "owner": "alice", "display_name": "Acme"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    put(
        test,
        root,
        "/api/v1/teams/acme/members/bob",
        json!({"role": "member"}),
    )
    .await;
    let (status, answer) = post(
        test,
        root,
        "/api/v1/catalogs",
        json!({"name": "acme-notes", "owner": "acme", "display_name": "Acme notes",
               "description": "Notes for Acme"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    put(
        test,
        root,
        "/api/v1/catalogs/acme-notes/member-access",
        json!({"access": "read"}),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn user_view_has_a_label() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    let (status, named) = post(
        &test,
        &root,
        "/api/v1/users",
        json!({"name": "alice", "display_name": "Alice Example"}),
    )
    .await;
    post(&test, &root, "/api/v1/users", json!({"name": "bob"})).await;
    let (_, by_email) = send(
        &test,
        "PUT",
        "/api/v1/users/bob/email",
        Some(&root),
        Some(json!({"email": "bob.b@example.com"})),
    )
    .await;
    let (_, listed) = send(&test, "GET", "/api/v1/users", Some(&root), None).await;

    assert_eq!(status, StatusCode::CREATED, "{named}");
    assert_eq!(named["label"], "Alice Example");
    assert_eq!(by_email["label"], "bob.b", "the email's local part");
    assert!(
        listed["users"]
            .as_array()
            .unwrap()
            .iter()
            .all(|user| user["label"].is_string()),
        "every listed user has a label: {listed}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn team_view_labels_a_personal_team_after_its_user() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    acme(&test, &root).await;

    let (_, personal) = send(&test, "GET", "/api/v1/teams/alice", Some(&root), None).await;
    let (_, shared) = send(&test, "GET", "/api/v1/teams/acme", Some(&root), None).await;
    let (_, listed) = send(&test, "GET", "/api/v1/teams", Some(&root), None).await;

    assert_eq!(personal["label"], "Alice Example's team");
    assert_eq!(personal["members"][0]["label"], "Alice Example");
    assert_eq!(shared["label"], "Acme");
    let members: Vec<(&str, &str)> = shared["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["user"].as_str().unwrap(), m["label"].as_str().unwrap()))
        .collect();
    assert_eq!(
        members,
        [
            ("alice", "Alice Example"),
            ("bob", user_label("", None).as_str())
        ]
    );
    assert!(listed["teams"]
        .as_array()
        .unwrap()
        .iter()
        .all(|team| team["label"].is_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn catalog_summary_has_display_name_and_label() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    acme(&test, &root).await;

    let (_, listing) = send(&test, "GET", "/api/v1/catalogs", Some(&root), None).await;
    let find = |name: &str| -> Value {
        listing["catalogs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name} in {listing}"))
    };

    let named = find("acme-notes");
    assert_eq!(named["display_name"], "Acme notes");
    assert_eq!(named["label"], "Acme notes");
    assert_eq!(named["owner_team_label"], "Acme");
    // `main` has only a description: the stored display name stays empty.
    let plain = find("main");
    assert_eq!(plain["display_name"], "");
    assert_eq!(plain["description"], "");
    assert_eq!(plain["label"], crate::labels::catalog_label("", ""));
    assert_eq!(
        plain["owner_team_label"],
        user_label("", None).as_str().to_string() + "'s team"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn patch_me_sets_my_display_name_and_refuses_the_rest() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    acme(&test, &root).await;
    let legacy = crate::store::keys::insert_legacy_test_key(
        test.app.pool(),
        "legacy",
        Access::Admin,
        "legacy",
        chrono::Utc::now(),
    )
    .await;
    test.app.poll().await.unwrap();
    let mut bob = Browser::session_for(&test, "bob").await;

    let (status, set) = patch(
        &mut bob,
        &test,
        "/api/v1/me",
        json!({"display_name": " Bob B "}),
    )
    .await;
    let (_, again) = send(&test, "GET", "/api/v1/whoami", Some(&root), None).await;
    let (long, refused) = patch(
        &mut bob,
        &test,
        "/api/v1/me",
        json!({"display_name": "x".repeat(81)}),
    )
    .await;
    let (unknown, _) = patch(
        &mut bob,
        &test,
        "/api/v1/me",
        json!({"display_name": "B", "admin": true}),
    )
    .await;
    let (no_user, no_user_body) = send(
        &test,
        "PATCH",
        "/api/v1/me",
        Some(&legacy),
        Some(json!({"display_name": "Nobody"})),
    )
    .await;
    // A key that belongs to a user is not that person's console session.
    let (key_of_user, _) = send(
        &test,
        "PATCH",
        "/api/v1/me",
        Some(&root),
        Some(json!({"display_name": "Root"})),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{set}");
    assert_eq!(set["display_name"], "Bob B");
    assert_eq!(set["label"], "Bob B");
    assert_eq!(
        again["user_label"],
        user_label("", None).as_str(),
        "root's own label"
    );
    assert_eq!(long, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["limit"], "display_name");
    assert_eq!(unknown, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(no_user, StatusCode::NOT_FOUND, "{no_user_body}");
    assert_eq!(key_of_user, StatusCode::FORBIDDEN);
    let (_, me) = bob
        .send(&test, "GET", "/api/v1/me", Payload::Empty)
        .await
        .parts();
    assert_eq!(me["display_name"], "Bob B", "the refusals changed nothing");
}

#[tokio::test(flavor = "multi_thread")]
async fn whoami_names_the_user_by_label() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    acme(&test, &root).await;
    let mut alice = Browser::session_for(&test, "alice").await;

    let reply = alice
        .send(&test, "GET", "/api/v1/whoami", Payload::Empty)
        .await;

    assert_eq!(reply.json()["user_label"], "Alice Example");
}

#[tokio::test(flavor = "multi_thread")]
async fn account_lists_owned_team_details() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    acme(&test, &root).await;
    let mut alice = Browser::session_for(&test, "alice").await;
    let mut bob = Browser::session_for(&test, "bob").await;

    let mine = alice
        .send(&test, "GET", "/api/v1/me", Payload::Empty)
        .await
        .json();
    let theirs = bob
        .send(&test, "GET", "/api/v1/me", Payload::Empty)
        .await
        .json();

    assert_eq!(mine["label"], "Alice Example");
    assert_eq!(mine["owned_teams"], json!(["acme", "alice"]), "unchanged");
    assert_eq!(
        mine["owned_team_details"],
        json!([
            {"name": "acme", "display_name": "Acme", "label": "Acme", "personal": false},
            {"name": "alice", "display_name": "Alice Example", "label": "Alice Example's team", "personal": true},
        ])
    );
    assert_eq!(
        theirs["owned_teams"],
        json!(["bob"]),
        "a member owns only its own team"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn patch_user_is_for_managers_only() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    let keys = keys(&test).await;
    acme(&test, &root).await;
    let mut bob = Browser::session_for(&test, "bob").await;
    let uri = "/api/v1/users/alice";
    let body = |name: &str| Some(json!({"display_name": name}));

    let (admin, view) = send(&test, "PATCH", uri, Some(&root), body("Alice B")).await;
    let (scoped, _) = send(&test, "PATCH", uri, Some(&keys.admin), body("Hacked")).await;
    let (read, _) = send(&test, "PATCH", uri, Some(&keys.read), body("Hacked")).await;
    let (session, _) = patch(&mut bob, &test, uri, json!({"display_name": "Hacked"})).await;
    let (unknown, _) = send(
        &test,
        "PATCH",
        "/api/v1/users/nobody",
        Some(&root),
        body("X"),
    )
    .await;
    let (bad, refused) = send(&test, "PATCH", uri, Some(&root), body("a\nb")).await;
    let (cleared, view_cleared) = send(&test, "PATCH", uri, Some(&root), body("")).await;

    assert_eq!(admin, StatusCode::OK, "{view}");
    assert_eq!(view["label"], "Alice B");
    assert_eq!(
        scoped,
        StatusCode::FORBIDDEN,
        "a scoped admin is no manager"
    );
    assert_eq!(read, StatusCode::FORBIDDEN);
    assert_eq!(session, StatusCode::FORBIDDEN, "not even for one's own row");
    assert_eq!(unknown, StatusCode::NOT_FOUND);
    assert_eq!(bad, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(cleared, StatusCode::OK);
    assert_eq!(view_cleared["display_name"], "");
}

#[tokio::test(flavor = "multi_thread")]
async fn patch_team_owner_session_ok_member_and_stranger_403_personal_409() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    acme(&test, &root).await;
    post(&test, &root, "/api/v1/users", json!({"name": "carol"})).await;
    let mut alice = Browser::session_for(&test, "alice").await;
    let mut bob = Browser::session_for(&test, "bob").await;
    let mut carol = Browser::session_for(&test, "carol").await;
    let uri = "/api/v1/teams/acme";
    let rename = |name: &str| json!({"display_name": name});

    let (owner, view) = patch(&mut alice, &test, uri, rename("Acme Ltd")).await;
    let (member, _) = patch(&mut bob, &test, uri, rename("Hacked")).await;
    let (stranger, _) = patch(&mut carol, &test, uri, rename("Hacked")).await;
    let (personal, _) = patch(&mut alice, &test, "/api/v1/teams/alice", rename("Mine")).await;
    let (other_personal, _) = patch(&mut bob, &test, "/api/v1/teams/alice", rename("Mine")).await;
    let (admin, _) = send(&test, "PATCH", uri, Some(&root), Some(rename("Acme Inc"))).await;
    let (unknown_admin, _) = send(
        &test,
        "PATCH",
        "/api/v1/teams/nope",
        Some(&root),
        Some(rename("X")),
    )
    .await;
    let (unknown_session, _) = patch(&mut alice, &test, "/api/v1/teams/nope", rename("X")).await;
    let (_, now) = send(&test, "GET", uri, Some(&root), None).await;

    assert_eq!(owner, StatusCode::OK, "{view}");
    assert_eq!(view["label"], "Acme Ltd");
    assert_eq!(view["members"][0]["label"], "Alice Example");
    assert_eq!(
        member,
        StatusCode::FORBIDDEN,
        "a member who is not an owner"
    );
    assert_eq!(stranger, StatusCode::FORBIDDEN, "another user's team");
    assert_eq!(
        personal,
        StatusCode::CONFLICT,
        "a personal team follows its user"
    );
    assert_eq!(other_personal, StatusCode::CONFLICT);
    assert_eq!(admin, StatusCode::OK);
    assert_eq!(unknown_admin, StatusCode::NOT_FOUND);
    assert_eq!(unknown_session, StatusCode::NOT_FOUND);
    assert_eq!(
        now["display_name"], "Acme Inc",
        "only the permitted writes landed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_below_a_manager_cannot_patch_a_team() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    let keys = keys(&test).await;
    acme(&test, &root).await;
    let body = Some(json!({"display_name": "Hacked"}));

    let (scoped, _) = send(
        &test,
        "PATCH",
        "/api/v1/teams/acme",
        Some(&keys.admin),
        body.clone(),
    )
    .await;
    let (write, _) = send(
        &test,
        "PATCH",
        "/api/v1/teams/acme",
        Some(&keys.write),
        body,
    )
    .await;

    assert_eq!(scoped, StatusCode::FORBIDDEN);
    assert_eq!(write, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn patch_catalog_owner_session_may_rename_its_teams_catalog() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    acme(&test, &root).await;
    post(&test, &root, "/api/v1/users", json!({"name": "carol"})).await;
    let mut alice = Browser::session_for(&test, "alice").await;
    let mut bob = Browser::session_for(&test, "bob").await;
    let mut carol = Browser::session_for(&test, "carol").await;
    let uri = "/api/v1/catalogs/acme-notes";

    let (owner, answer) = patch(
        &mut alice,
        &test,
        uri,
        json!({"display_name": " Acme wiki "}),
    )
    .await;
    let (member, _) = patch(&mut bob, &test, uri, json!({"display_name": "Hacked"})).await;
    let (stranger, _) = patch(&mut carol, &test, uri, json!({"display_name": "Hacked"})).await;
    let (none, empty) = patch(&mut alice, &test, uri, json!({})).await;
    let (bad, _) = patch(&mut alice, &test, uri, json!({"display_name": "a\u{7}b"})).await;
    let (_, listing) = send(&test, "GET", "/api/v1/catalogs", Some(&root), None).await;

    assert_eq!(owner, StatusCode::OK, "{answer}");
    // A strict superset of the old answer: `previous` stays the old
    // description string.
    assert_eq!(
        answer,
        json!({
            "name": "acme-notes",
            "previous": "Notes for Acme",
            "description": "Notes for Acme",
            "previous_display_name": "Acme notes",
            "display_name": "Acme wiki",
            "label": "Acme wiki",
        })
    );
    assert_eq!(
        member,
        StatusCode::FORBIDDEN,
        "a member who is not an owner"
    );
    assert_eq!(stranger, StatusCode::FORBIDDEN, "another team's catalog");
    assert_eq!(none, StatusCode::UNPROCESSABLE_ENTITY, "{empty}");
    assert_eq!(empty["limit"], "body");
    assert_eq!(bad, StatusCode::UNPROCESSABLE_ENTITY);
    let acme = listing["catalogs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "acme-notes")
        .unwrap();
    assert_eq!(acme["label"], "Acme wiki", "polled at once");
    assert_eq!(acme["display_name"], "Acme wiki");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_suspended_teams_owner_cannot_edit_its_catalogs_label() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    post(&test, &root, "/api/v1/users", json!({"name": "carol"})).await;
    suspend_catalogs(&test, &["main"]).await;
    let mut owner = Browser::session_for(&test, TEST_USER).await;
    let mut carol = Browser::session_for(&test, "carol").await;
    let uri = "/api/v1/catalogs/main";

    let (refused, body) = patch(&mut owner, &test, uri, json!({"display_name": "Mine"})).await;
    let (outsider, outsider_body) =
        patch(&mut carol, &test, uri, json!({"display_name": "Mine"})).await;
    let (admin, answer) = send(
        &test,
        "PATCH",
        uri,
        Some(&root),
        Some(json!({"display_name": "Ops"})),
    )
    .await;

    assert_eq!(refused, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "Suspended");
    assert_eq!(outsider, StatusCode::FORBIDDEN);
    assert_eq!(
        outsider_body["error"], "Forbidden",
        "an outsider never learns the catalog is suspended: {outsider_body}"
    );
    assert_eq!(
        admin,
        StatusCode::OK,
        "the operator may still edit it: {answer}"
    );
    let row = crate::store::catalogs::find(test.app.pool(), "main")
        .await
        .unwrap();
    assert_eq!(row.display_name, "Ops", "only the admin's edit landed");
}

#[tokio::test(flavor = "multi_thread")]
async fn patch_catalog_admin_may_still_change_the_description() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    let keys = keys(&test).await;
    let uri = "/api/v1/catalogs/main";

    let (status, answer) = send(
        &test,
        "PATCH",
        uri,
        Some(&keys.admin),
        Some(json!({"description": "New text"})),
    )
    .await;
    let (read, _) = send(
        &test,
        "PATCH",
        uri,
        Some(&keys.read),
        Some(json!({"description": "x"})),
    )
    .await;
    let (write, _) = send(
        &test,
        "PATCH",
        uri,
        Some(&keys.write),
        Some(json!({"description": "x"})),
    )
    .await;
    let (stranger, _) = send(
        &test,
        "PATCH",
        uri,
        Some(&keys.stranger),
        Some(json!({"description": "x"})),
    )
    .await;
    let (unknown, _) = send(
        &test,
        "PATCH",
        "/api/v1/catalogs/nope",
        Some(&root),
        Some(json!({"description": "x"})),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["name"], "main");
    assert!(
        answer["previous"].is_string(),
        "previous is the old description: {answer}"
    );
    assert_eq!(answer["description"], "New text");
    assert_eq!(
        answer["display_name"], answer["previous_display_name"],
        "left as it was"
    );
    assert_eq!(answer["label"], "New text");
    assert_eq!(read, StatusCode::FORBIDDEN);
    assert_eq!(write, StatusCode::FORBIDDEN);
    assert_eq!(
        stranger,
        StatusCode::NOT_FOUND,
        "a key that does not reach it"
    );
    assert_eq!(unknown, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn create_catalog_answers_with_its_labels() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;

    let (status, created) = post(
        &test,
        &root,
        "/api/v1/catalogs",
        json!({"name": "work", "display_name": "Work notes", "owner": TEST_TEAM}),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["display_name"], "Work notes");
    assert_eq!(created["label"], "Work notes");
    assert_eq!(
        created["owner_team_label"],
        user_label("", None).as_str().to_string() + "'s team"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn labels_scope() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    let keys = keys(&test).await;
    acme(&test, &root).await;
    let mut bob = Browser::session_for(&test, "bob").await;
    let unnamed = user_label("", None);
    let keys_of = |view: &Value, kind: &str| -> Vec<String> {
        view[kind].as_object().unwrap().keys().cloned().collect()
    };

    let (status, all) = send(&test, "GET", "/api/v1/labels", Some(&root), None).await;
    let member = bob
        .send(&test, "GET", "/api/v1/labels", Payload::Empty)
        .await
        .json();
    let (_, scoped) = send(&test, "GET", "/api/v1/labels", Some(&keys.stranger), None).await;

    // An every-catalog admin sees everything.
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(keys_of(&all, "users"), ["alice", "bob", TEST_USER]);
    assert_eq!(keys_of(&all, "teams"), ["acme", "alice", "bob", TEST_TEAM]);
    assert_eq!(keys_of(&all, "catalogs"), ["acme-notes", "main", "other"]);
    assert_eq!(all["users"]["alice"], "Alice Example");
    assert_eq!(all["teams"]["acme"], "Acme");
    assert_eq!(all["catalogs"]["acme-notes"], "Acme notes");
    // A session member sees itself, its teams, what it reaches, and those
    // catalogs' owners; nobody else.
    assert_eq!(member["users"], json!({"bob": unnamed}));
    assert_eq!(keys_of(&member, "teams"), ["acme", "bob"]);
    assert_eq!(member["teams"]["acme"], "Acme");
    assert_eq!(member["catalogs"], json!({"acme-notes": "Acme notes"}));
    // A scoped key sees its own catalogs and their owner teams.
    assert_eq!(keys_of(&scoped, "catalogs"), ["other"]);
    assert_eq!(keys_of(&scoped, "teams"), [TEST_TEAM]);
    assert_eq!(keys_of(&scoped, "users"), [TEST_USER]);
}

#[tokio::test(flavor = "multi_thread")]
async fn labels_tell_two_catalogs_with_one_label_apart() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    acme(&test, &root).await;
    post(
        &test,
        &root,
        "/api/v1/catalogs",
        json!({"name": "twin", "display_name": "Acme notes", "owner": TEST_TEAM}),
    )
    .await;

    let (_, all) = send(&test, "GET", "/api/v1/labels", Some(&root), None).await;

    let first = all["catalogs"]["acme-notes"].as_str().unwrap();
    let second = all["catalogs"]["twin"].as_str().unwrap();
    assert_ne!(first, second, "{all}");
    assert!(first.contains("Acme"), "{first}");
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_are_audited_by_length_not_text() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    acme(&test, &root).await;
    let mut alice = Browser::session_for(&test, "alice").await;

    patch(
        &mut alice,
        &test,
        "/api/v1/me",
        json!({"display_name": "Alice Q"}),
    )
    .await;
    patch(
        &mut alice,
        &test,
        "/api/v1/teams/acme",
        json!({"display_name": "Acme Q"}),
    )
    .await;
    patch(
        &mut alice,
        &test,
        "/api/v1/catalogs/acme-notes",
        json!({"display_name": "Notes Q"}),
    )
    .await;

    let user =
        crate::store::audit::testing::entries(test.app.pool(), "user.display_name_changed").await;
    let team =
        crate::store::audit::testing::entries(test.app.pool(), "team.display_name_changed").await;
    let catalog =
        crate::store::audit::testing::entries(test.app.pool(), "catalog.display_name_changed")
            .await;
    assert_eq!(user.last().unwrap().details, json!({"length": 7}));
    assert_eq!(team.last().unwrap().details, json!({"length": 6}));
    assert_eq!(catalog.last().unwrap().details, json!({"length": 7}));
    assert_eq!(user.last().unwrap().source, "console");
}

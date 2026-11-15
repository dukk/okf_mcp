//! The Audit page (M14 Task 8c): an admin reads and filters everything and
//! exports it; a team owner picks one of its teams and sees operators as
//! "an operator"; and, per `tools/agentic/lessons.md`' read rule, only the
//! newest table load draws, "Load more" appends only to it, and a failed
//! load keeps its rows and its reason on screen.

use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;
use serde_json::{json, Value};

use crate::harness::drive;
use crate::self_service::{console_base, login_server, member, EMAIL, PASSWORD};
use crate::support::server::{files, TestServer, OWNER};

async fn server() -> TestServer {
    TestServer::start(
        vec![("main", files(&[("index.md", "# Main\n")]))],
        Config::default(),
    )
    .await
}

/// Sends one API request as `key` and fails the test unless it succeeds.
async fn call(server: &TestServer, key: &str, method: &str, path: &str, body: Value) {
    let response = reqwest::Client::new()
        .request(
            method.parse().expect("an HTTP method"),
            format!("{}{path}", server.base),
        )
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .expect("send the request");
    assert!(
        response.status().is_success(),
        "{method} {path}: {}",
        response.status()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_filters_the_log_by_action_and_exports_it() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.click("#user-new");
        ui.fill("#user-form input[name=name]", "alice");
        ui.click("#user-form button[type=submit]");
        ui.wait_visible("[data-test=user-alice]");
        ui.open("/audit");
        ui.wait_text("#audit-table", "user.created");
        ui.select("#audit-action", "user.created");
        ui.wait_text("#audit-table", "alice");
        ui.wait_text_gone("#audit-table", "catalog.created");
        // The user links to its drawer, which lists its sign-ins.
        ui.click("#audit-table a[href='#/users/alice']");
        ui.wait_visible("#drawer");
        ui.wait_text("#drawer", "alice");
        ui.click("#drawer-close");
        ui.open("/audit");
        ui.select("#audit-action", "user.created");
        ui.wait_text_gone("#audit-table", "catalog.created");
        // The export asks with the filters shown, through the console's own
        // fetch (and so its credential), not a plain link.
        ui.hold_next("GET", "/api/v1/audit/export?action=user.created");
        ui.click("#audit-export");
        assert_eq!(ui.held_matches(), 1);
        ui.release_held();
        ui.wait_hidden("#audit-error");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_reads_its_team_and_sees_an_operator() {
    // Signing in with a password needs the login server (a `localhost`
    // origin); alice is a member there with read on `main`.
    let server = login_server().await;
    member(&server, Access::Read).await;
    let root = server.key("console-admin", Access::Admin, &[], None).await;
    // An operator (an admin key) makes alice's team and adds a member.
    call(
        &server,
        &root,
        "POST",
        "/api/v1/teams",
        json!({"name": "family", "owner": "alice"}),
    )
    .await;
    call(
        &server,
        &root,
        "PUT",
        &format!("/api/v1/teams/family/members/{OWNER}"),
        json!({"role": "member"}),
    )
    .await;
    drive(console_base(&server), move |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.wait_visible("[data-nav=audit]");
        ui.open("/audit");
        ui.select("#audit-team", "family");
        ui.wait_text("#audit-table", "team.member_added");
        ui.wait_text("#audit-table", "an operator");
        ui.wait_text_gone("#audit-table", "console-admin");
        // The export carries the owner's team.
        ui.hold_next("GET", "/api/v1/audit/export?team=family");
        ui.click("#audit-export");
        assert_eq!(ui.held_matches(), 1);
        ui.release_held();
        ui.wait_hidden("#audit-error");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_table_load_never_draws_over_a_newer_filter() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    call(
        &server,
        &admin,
        "POST",
        "/api/v1/users",
        json!({"name": "alice"}),
    )
    .await;
    call(
        &server,
        &admin,
        "POST",
        "/api/v1/teams",
        json!({"name": "family", "owner": "alice"}),
    )
    .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/audit");
        ui.wait_text("#audit-table", "team.created");
        ui.wait_text("#audit-table", "user.created");
        // The first filter's answer is held; the second's arrives first,
        // and is drawn (the unfiltered rows' user.created is gone).
        ui.hold_answer("GET", "/api/v1/audit?action=user.created", || {
            ui.select("#audit-action", "user.created")
        });
        ui.select("#audit-action", "team.created");
        ui.wait_text_gone("#audit-table", "user.created");
        ui.wait_text("#audit-table", "family");
        ui.release_held();
        // release_held gave the page time to draw the late answer.
        let table = ui.text("#audit-table");
        assert!(
            table.contains("team.created") && !table.contains("user.created"),
            "the older filter's answer drew over the newer one: {table}"
        );
        // A failed load keeps the rows, marked, with its reason on screen,
        // and Refresh still works.
        ui.fail_next(
            "GET",
            "/api/v1/audit?action=team.created",
            503,
            "the log is away",
        );
        ui.click("#audit-refresh");
        ui.wait_visible("#audit-error");
        ui.wait_text("#audit-error", "the log is away");
        ui.wait_visible("#audit-stale");
        assert!(ui.text("#audit-table").contains("family"));
        ui.click("#audit-refresh");
        ui.wait_hidden("#audit-error");
        ui.wait_hidden("#audit-stale");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn load_more_appends_only_to_the_newest_table_load() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    // More user.created entries than one page (100) holds.
    for index in 0..101 {
        call(
            &server,
            &admin,
            "POST",
            "/api/v1/users",
            json!({"name": format!("user-{index}")}),
        )
        .await;
    }
    call(
        &server,
        &admin,
        "POST",
        "/api/v1/teams",
        json!({"name": "family", "owner": "user-0"}),
    )
    .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/audit");
        ui.select("#audit-action", "user.created");
        ui.wait_visible("#audit-more");
        // The second page's answer is held while the filter changes.
        ui.hold_answer("GET", "&action=user.created", || ui.click("#audit-more"));
        ui.select("#audit-action", "team.created");
        ui.wait_text("#audit-table", "family");
        ui.release_held();
        let table = ui.text("#audit-table");
        assert!(
            !table.contains("user.created"),
            "a page of the previous filter was appended: {table}"
        );
    })
    .await;
}

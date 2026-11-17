//! Two replicas over one Postgres and one Redis (milestone 16). Every test
//! sets `poll_interval = 1h` (`TestServer::start_shared`): what passes was
//! carried by Redis announcements, not by the poll loop. The MCP session
//! tests (milestone 16 Task 5) open a session on one replica and use it, or
//! are refused, on the other.

mod support;

use std::time::Duration;

use chrono::Utc;
use okf_mcp::archive::Files;
use okf_mcp::config::{Config, OAuthConfig};
use okf_mcp::store::audit::AuditActor;
use okf_mcp::store::keys::{self, Access, Grants, NewKey};
use okf_mcp::store::users::{self, NewUser};
use serde_json::json;
use support::mcp_raw::{
    delete_on, first_event_data, notify_on, open_session, post_on, stream_on, TOOLS_LIST,
};
use support::oauth::{sign_in_code, CimdHost, OAuthClient};
use support::redis::TestRedis;
use support::server::{call, files, Sibling, TestServer, OWNER};

/// Longest any wait for a replica to catch up may take.
const WAIT: Duration = Duration::from_secs(10);

fn pg() -> Vec<(&'static str, Files)> {
    vec![("main", files(&[("a.md", "---\ntype: Note\n---\n# A\n")]))]
}

/// Retries `check` every 50 ms until it is true, or panics naming `what`
/// after [`WAIT`].
async fn eventually(what: &str, check: impl Fn() -> bool) {
    let waited = tokio::time::timeout(WAIT, async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "timed out waiting until {what}");
}

/// Two replicas on one Redis, with an admin key known to both.
async fn pair(redis: &TestRedis) -> (TestServer, Sibling, String) {
    let a = TestServer::start_shared(pg(), redis, |_| Config::default()).await;
    let b = a.sibling(redis, |_| Config::default()).await;
    let admin = a.key("admin", Access::Admin, &[], None).await;
    // B has not polled: wait until it holds the admin key through Redis.
    eventually("B knows the admin key", || {
        b.app.keys().find(&admin).is_some()
    })
    .await;
    (a, b, admin)
}

/// `POST`s, `PUT`s or `DELETE`s `path` on `base` as `key`, asserting success.
async fn api(method: reqwest::Method, base: &str, path: &str, key: &str, body: serde_json::Value) {
    let mut request = reqwest::Client::new()
        .request(method, format!("{base}{path}"))
        .bearer_auth(key);
    if !body.is_null() {
        request = request.json(&body);
    }
    let answer = request.send().await.expect("api call");
    assert!(answer.status().is_success(), "{path}: {}", answer.status());
}

/// Revokes through the API on A: the key is refused on B at once.
async fn revoked_key_is_refused_on_the_other(redis: &TestRedis) {
    let (a, b, admin) = pair(redis).await;
    let key = a.key("reader", Access::Read, &[], None).await;
    eventually("B knows the new key", || b.app.keys().find(&key).is_some()).await;

    let id = b.app.keys().find(&key).expect("cached").key_id.clone();
    api(
        reqwest::Method::POST,
        &a.base,
        &format!("/api/v1/keys/{id}/revoke"),
        &admin,
        serde_json::Value::Null,
    )
    .await;

    eventually("B refuses the revoked key", || {
        b.app.keys().find(&key).is_none()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_revoked_on_one_replica_is_refused_on_the_other_at_once() {
    let redis = TestRedis::start().await;
    revoked_key_is_refused_on_the_other(&redis).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_revoked_on_one_replica_is_refused_on_the_other_in_cluster_mode() {
    let redis = TestRedis::start_cluster().await;
    revoked_key_is_refused_on_the_other(&redis).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_user_deleted_on_one_replica_loses_access_on_the_other() {
    let redis = TestRedis::start().await;
    let (a, b, admin) = pair(&redis).await;
    let user = NewUser {
        name: "alice",
        display_name: "",
    };
    users::create(&a.db.pool, &user, &AuditActor::cli(), Utc::now())
        .await
        .expect("create alice");
    let new = NewKey {
        name: "alice-key",
        access: Access::Read,
        maintenance_report: false,
        grants: Grants::All,
        default_catalog: None,
        actor: "alice",
        user: "alice",
        self_issued: false,
        expires_at: None,
        levels: &[],
    };
    let key = keys::create(&a.db.pool, &new, &AuditActor::cli(), Utc::now())
        .await
        .expect("create alice's key");
    a.app.poll().await.expect("A's poll");
    eventually("B knows alice's key", || b.app.keys().find(&key).is_some()).await;

    api(
        reqwest::Method::DELETE,
        &a.base,
        "/api/v1/users/alice",
        &admin,
        serde_json::Value::Null,
    )
    .await;

    eventually("B no longer finds alice's key", || {
        b.app.keys().find(&key).is_none()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_team_suspended_on_one_replica_closes_its_catalogs_on_the_other() {
    let redis = TestRedis::start().await;
    let (a, b, admin) = pair(&redis).await;
    let reader = a.key("reader", Access::Read, &[], None).await;
    eventually("B knows the reader", || {
        b.app.keys().find(&reader).is_some()
    })
    .await;
    let args = json!({"catalog": "main", "path": "a"});
    let client = tokio::time::timeout(WAIT, async {
        let url = b.mcp_url();
        // `client_at` is a method of the first replica's harness; the URL is B's.
        a.client_at(&reader, &url, &[]).await
    })
    .await
    .expect("connecting to B in time")
    .expect("a client on B");
    let before = call(&client, "get_concept", args.clone()).await;
    assert_ne!(before.is_error, Some(true), "{before:?}");

    api(
        reqwest::Method::PUT,
        &a.base,
        &format!("/api/v1/teams/{OWNER}/suspension"),
        &admin,
        json!({"suspended": true, "notice": "Contact the operator."}),
    )
    .await;

    let refused = tokio::time::timeout(WAIT, async {
        loop {
            let answer = call(&client, "get_concept", args.clone()).await;
            if answer.is_error == Some(true) {
                return answer;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("B refuses the suspended team's catalog in time");
    let text = format!("{refused:?}").to_lowercase();
    assert!(text.contains("suspend"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grant_revoked_on_one_replica_is_refused_on_the_other() {
    let redis = TestRedis::start().await;
    let config = |base: &str| Config {
        public_url: Some(base.to_string()),
        oauth: OAuthConfig {
            cimd_allow_private: true,
            ..OAuthConfig::default()
        },
        ..Config::default()
    };
    let a = TestServer::start_shared(pg(), &redis, config).await;
    let b = a.sibling(&redis, config).await;
    let host = CimdHost::start().await;
    let client = OAuthClient::new(&a, &host.client_id, &host.redirect_uri);
    let resource = client.discover().await.resource;
    let code = client
        .authorize(&sign_in_code(&a, Access::Read, &["main"]).await, &resource)
        .await;
    let tokens = client.exchange(&code, &resource).await;
    assert_eq!(tokens.status, 200, "{}", tokens.body);
    let oauth = a.app.oauth().expect("OAuth is on");
    let grant_id = oauth
        .access
        .verify(&a.app, &oauth.settings.resource, &tokens.access())
        .await
        .expect("verify")
        .expect("a principal")
        .key_id
        .clone();
    a.app.poll().await.expect("A's poll");
    eventually("B knows the grant", || {
        b.app.keys().find_grant(&grant_id).is_some()
    })
    .await;

    keys::revoke_by_id(&a.db.pool, &grant_id, &AuditActor::cli(), Utc::now())
        .await
        .expect("revoke the grant");
    // What `settle` does after a change made through the API.
    a.app.poll().await.expect("A's poll");

    eventually("B refuses the revoked grant", || {
        b.app.keys().find_grant(&grant_id).is_none()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_still_announces_after_a_non_announcing_reload_saw_it_first() {
    let redis = TestRedis::start().await;
    let (a, b, _admin) = pair(&redis).await;
    let key = a.key("reader", Access::Read, &[], None).await;
    eventually("B knows the key", || b.app.keys().find(&key).is_some()).await;
    let id = a.app.keys().find(&key).expect("cached").key_id.clone();

    // A change made through the store, as the CLI would: nothing settles yet.
    keys::revoke_by_id(&a.db.pool, &id, &AuditActor::cli(), Utc::now())
        .await
        .expect("revoke");
    // A reload that does not announce still loads the new generation.
    a.app.catch_up_keys().await;
    assert!(a.app.keys().find(&key).is_none(), "A loaded the change");
    // The poll `settle` makes next must still announce it.
    a.app.poll().await.expect("A's poll");

    eventually("B refuses the key A's silent reload saw first", || {
        b.app.keys().find(&key).is_none()
    })
    .await;
}

/// Revokes `key` on A through the store and polls A, which announces.
async fn revoke_on_a(a: &TestServer, key: &str) {
    let id = a.app.keys().find(key).expect("cached").key_id.clone();
    keys::revoke_by_id(&a.db.pool, &id, &AuditActor::cli(), Utc::now())
        .await
        .expect("revoke");
    a.app.poll().await.expect("A's poll");
}

#[tokio::test(flavor = "multi_thread")]
// Whether B hears the revoke from the resubscribed stream or from its
// `Subscribed` reload depends on timing; the missed-announcement ordering is
// pinned by `shared::announce::catch_up_tests`. This one checks the whole
// server keeps announcing to a subscriber whose connection was killed.
async fn a_subscriber_whose_connection_is_killed_still_learns_of_a_revoke() {
    let redis = TestRedis::start().await;
    let (a, b, _admin) = pair(&redis).await;
    let key = a.key("reader", Access::Read, &[], None).await;
    eventually("B knows the key", || b.app.keys().find(&key).is_some()).await;

    kill_pubsub_connections(&redis).await;
    revoke_on_a(&a, &key).await;

    eventually("B refuses the key after resubscribing", || {
        b.app.keys().find(&key).is_none()
    })
    .await;
}

/// `CLIENT KILL TYPE pubsub`: ends every subscriber connection.
async fn kill_pubsub_connections(redis: &TestRedis) {
    let handle = support::server::redis_connect(redis).await;
    let mut connection = handle.connection();
    let _: redis::Value = redis::cmd("CLIENT")
        .arg("KILL")
        .arg("TYPE")
        .arg("pubsub")
        .query_async(&mut connection)
        .await
        .expect("client kill");
}

// ---- MCP sessions shared between replicas (Task 5) ----

/// Two replicas on one Redis with `config`, and a reader key (default catalog
/// `main`) that both know.
async fn session_pair(redis: &TestRedis, config: Config) -> (TestServer, Sibling, String) {
    let a = TestServer::start_shared(pg(), redis, |_| config.clone()).await;
    let b = a.sibling(redis, |_| config.clone()).await;
    let key = a.key("reader", Access::Read, &[], Some("main")).await;
    eventually("B knows the reader key", || {
        b.app.keys().find(&key).is_some()
    })
    .await;
    (a, b, key)
}

/// Opens a session on `mcp_url` and sends its `initialized` notification.
async fn open_ready(mcp_url: &str, key: &str) -> String {
    let session = open_session(mcp_url, key).await;
    assert_eq!(notify_on(mcp_url, key, &session).await, 202);
    session
}

/// `tools/list` on `session`: the status.
async fn tools_list(mcp_url: &str, key: &str, session: &str) -> reqwest::StatusCode {
    post_on(mcp_url, key, session, TOOLS_LIST).await.status()
}

/// The binding key and the state key of `session` on `redis`'s prefix.
fn session_keys(handle: &okf_mcp::shared::Redis, session: &str) -> (String, String) {
    let id = okf_mcp::shared::hashed(session);
    (handle.key("mcp:bind", &id), handle.key("mcp:state", &id))
}

/// Runs a raw command (test set-up and inspection).
async fn raw<T: redis::FromRedisValue>(handle: &okf_mcp::shared::Redis, cmd: &mut redis::Cmd) -> T {
    cmd.query_async(&mut handle.connection())
        .await
        .expect("a raw redis command")
}

/// How many of `keys` exist.
async fn existing(handle: &okf_mcp::shared::Redis, keys: &[&str]) -> i64 {
    let mut command = redis::cmd("EXISTS");
    for key in keys {
        command.arg(*key);
    }
    raw(handle, &mut command).await
}

/// Opens on A, then lists and calls a tool on B, which restores from Redis.
async fn session_moves_between_replicas(redis: &TestRedis) {
    let (a, b, key) = session_pair(redis, Config::default()).await;

    let session = open_ready(&a.mcp_url(), &key).await;
    let listed = post_on(&b.mcp_url(), &key, &session, TOOLS_LIST).await;
    let listed_status = listed.status();
    let listed_body = listed.text().await.expect("the tools/list body");
    let called = post_on(
        &b.mcp_url(),
        &key,
        &session,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_concept","arguments":{"path":"a"}}}"#,
    )
    .await;
    let called_status = called.status();
    let called_body = called.text().await.expect("the tools/call body");

    assert_eq!(listed_status, 200);
    let first: serde_json::Value =
        serde_json::from_str(first_event_data(&listed_body)).expect("a JSON event");
    assert!(first["result"]["tools"].is_array(), "{listed_body}");
    assert_eq!(called_status, 200);
    let answer: serde_json::Value =
        serde_json::from_str(first_event_data(&called_body)).expect("a JSON event");
    assert_eq!(answer["id"], 3, "{called_body}");
    assert!(answer["result"].is_object(), "{called_body}");
    assert_ne!(answer["result"]["isError"], json!(true), "{called_body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_opened_on_one_replica_serves_tools_on_the_other() {
    let redis = TestRedis::start().await;
    session_moves_between_replicas(&redis).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_opened_on_one_replica_serves_tools_on_the_other_in_cluster_mode() {
    let redis = TestRedis::start_cluster().await;
    session_moves_between_replicas(&redis).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn another_key_on_that_session_is_refused_on_either_replica() {
    let redis = TestRedis::start().await;
    let (a, b, owner) = session_pair(&redis, Config::default()).await;
    let intruder = a.key("intruder", Access::Read, &[], Some("main")).await;
    eventually("B knows the intruder key", || {
        b.app.keys().find(&intruder).is_some()
    })
    .await;
    let session = open_ready(&a.mcp_url(), &owner).await;

    let mut refusals = Vec::new();
    for url in [a.mcp_url(), b.mcp_url()] {
        refusals.push(tools_list(&url, &intruder, &session).await);
        refusals.push(stream_on(&url, &intruder, &session).await);
    }
    let own_on_b = tools_list(&b.mcp_url(), &owner, &session).await;

    assert_eq!(refusals, vec![reqwest::StatusCode::NOT_FOUND; 4]);
    assert_eq!(own_on_b, 200, "the owner is still admitted");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_binding_without_its_state_is_refused_on_every_replica() {
    let redis = TestRedis::start().await;
    let (a, b, key) = session_pair(&redis, Config::default()).await;
    let handle = support::server::redis_connect(&redis).await;
    let session = open_ready(&a.mcp_url(), &key).await;
    let (_, state) = session_keys(&handle, &session);

    // As an eviction would.
    let _: i64 = raw(&handle, redis::cmd("DEL").arg(&state)).await;
    // A still holds a local worker for the session: the script, not a
    // failed restore, must refuse it.
    let on_a = tools_list(&a.mcp_url(), &key, &session).await;
    let on_b = tools_list(&b.mcp_url(), &key, &session).await;

    assert_eq!(on_a, 404);
    assert_eq!(on_b, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_state_without_a_binding_is_never_restored() {
    let redis = TestRedis::start().await;
    let (a, b, key) = session_pair(&redis, Config::default()).await;
    let handle = support::server::redis_connect(&redis).await;
    let session = open_ready(&a.mcp_url(), &key).await;
    let (bind, _) = session_keys(&handle, &session);

    let _: i64 = raw(&handle, redis::cmd("DEL").arg(&bind)).await;
    let on_b = tools_list(&b.mcp_url(), &key, &session).await;
    let on_a = tools_list(&a.mcp_url(), &key, &session).await;

    assert_eq!(on_b, 404);
    assert_eq!(on_a, 404, "the binding is the check, not the local worker");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_keys_session_is_refused_on_every_replica() {
    let redis = TestRedis::start().await;
    let (a, b, admin) = pair(&redis).await;
    let key = a.key("reader", Access::Read, &[], Some("main")).await;
    eventually("B knows the reader key", || {
        b.app.keys().find(&key).is_some()
    })
    .await;
    let session = open_ready(&a.mcp_url(), &key).await;
    assert_eq!(tools_list(&b.mcp_url(), &key, &session).await, 200);

    let id = a.app.keys().find(&key).expect("cached").key_id.clone();
    api(
        reqwest::Method::POST,
        &a.base,
        &format!("/api/v1/keys/{id}/revoke"),
        &admin,
        serde_json::Value::Null,
    )
    .await;
    eventually("B refuses the revoked key", || {
        b.app.keys().find(&key).is_none()
    })
    .await;

    assert_eq!(tools_list(&a.mcp_url(), &key, &session).await, 401);
    assert_eq!(tools_list(&b.mcp_url(), &key, &session).await, 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_session_expires_in_redis() {
    let redis = TestRedis::start().await;
    let config = Config {
        session_idle_timeout: Duration::from_secs(2),
        ..Config::default()
    };
    let (a, b, key) = session_pair(&redis, config).await;
    let handle = support::server::redis_connect(&redis).await;
    let session = open_ready(&a.mcp_url(), &key).await;
    let (bind, state) = session_keys(&handle, &session);

    let bind_ttl: i64 = raw(&handle, redis::cmd("PTTL").arg(&bind)).await;
    let state_ttl: i64 = raw(&handle, redis::cmd("PTTL").arg(&state)).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let on_b = tools_list(&b.mcp_url(), &key, &session).await;
    let left = existing(&handle, &[&bind, &state]).await;

    assert!((1..=2000).contains(&bind_ttl), "{bind_ttl}");
    assert!((1..=2000).contains(&state_ttl), "{state_ttl}");
    assert_eq!(on_b, 404);
    assert_eq!(left, 0, "both keys expired");
}

#[tokio::test(flavor = "multi_thread")]
async fn use_on_one_replica_keeps_the_session_alive_for_the_other() {
    let redis = TestRedis::start().await;
    let config = Config {
        session_idle_timeout: Duration::from_secs(3),
        ..Config::default()
    };
    let (a, b, key) = session_pair(&redis, config).await;
    let session = open_ready(&a.mcp_url(), &key).await;

    let mut on_b = Vec::new();
    for _ in 0..5 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        on_b.push(tools_list(&b.mcp_url(), &key, &session).await);
    }
    // Five seconds without use on A: its own worker idled out, so A
    // restores the session from Redis, whose TTL B kept sliding.
    let on_a = tools_list(&a.mcp_url(), &key, &session).await;

    assert_eq!(on_b, vec![reqwest::StatusCode::OK; 5]);
    assert_eq!(on_a, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_ends_the_session_everywhere() {
    let redis = TestRedis::start().await;
    let (a, b, owner) = session_pair(&redis, Config::default()).await;
    let intruder = a.key("intruder", Access::Read, &[], Some("main")).await;
    eventually("B knows the intruder key", || {
        b.app.keys().find(&intruder).is_some()
    })
    .await;
    let handle = support::server::redis_connect(&redis).await;

    // Another key's DELETE on B is refused and releases nothing.
    let kept = open_ready(&a.mcp_url(), &owner).await;
    let (kept_bind, kept_state) = session_keys(&handle, &kept);
    let foreign_delete = delete_on(&b.mcp_url(), &intruder, &kept).await;
    let kept_left = existing(&handle, &[&kept_bind, &kept_state]).await;

    // B has never received a POST or GET for this session, so it holds no
    // local copy (review of PR #552).
    let session = open_ready(&a.mcp_url(), &owner).await;
    let (bind, state) = session_keys(&handle, &session);
    let deleted = delete_on(&b.mcp_url(), &owner, &session).await;
    let left = existing(&handle, &[&bind, &state]).await;
    let on_a = tools_list(&a.mcp_url(), &owner, &session).await;
    let on_b = tools_list(&b.mcp_url(), &owner, &session).await;

    assert_eq!(foreign_delete, 404);
    assert_eq!(kept_left, 2, "another key's DELETE releases nothing");
    assert_eq!(deleted, 202);
    assert_eq!(left, 0, "both keys are gone");
    assert_eq!(on_a, 404);
    assert_eq!(on_b, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn redis_down_answers_503_not_a_fallback() {
    let redis = TestRedis::start().await;
    let (a, _b, key) = session_pair(&redis, Config::default()).await;
    let session = open_ready(&a.mcp_url(), &key).await;
    if redis.stop().await.is_none() {
        // A shared server: it cannot be stopped for one test.
        return;
    }

    // A holds the session locally, but the binding check is authoritative.
    let status = tokio::time::timeout(
        Duration::from_secs(30),
        tools_list(&a.mcp_url(), &key, &session),
    )
    .await
    .expect("an answer within 30 s while Redis is down");

    assert_eq!(status, 503);
}

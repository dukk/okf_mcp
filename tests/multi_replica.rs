//! Two replicas over one Postgres and one Redis (milestone 16). Every test
//! sets `poll_interval = 1h` (`TestServer::start_shared`): what passes was
//! carried by Redis announcements, not by the poll loop.

mod support;

use std::time::Duration;

use chrono::Utc;
use okf_mcp::archive::Files;
use okf_mcp::config::{Config, OAuthConfig};
use okf_mcp::store::audit::AuditActor;
use okf_mcp::store::keys::{self, Access, Grants, NewKey};
use okf_mcp::store::users::{self, NewUser};
use serde_json::json;
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
async fn a_subscriber_whose_connection_is_killed_catches_up_when_it_resubscribes() {
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

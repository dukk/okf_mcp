//! Raw MCP over HTTP with `reqwest`: one request at a time, with the session
//! header set by hand, for tests of the session binding (`tests/server.rs`)
//! and of sessions shared between replicas (`tests/multi_replica.rs`). Every
//! helper takes the `/mcp` URL, so it works against any replica.

// Each crate that includes this file uses a different part of it.
#![allow(dead_code)]

use reqwest::StatusCode;

/// An `initialize` request at protocol version 2025-11-25.
pub const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;

/// The `notifications/initialized` notification.
pub const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

/// `tools/list`, id 2.
pub const TOOLS_LIST: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;

/// The first SSE event's `data` field of a response body, if any.
///
/// # Arguments
/// * `body` - an event-stream body.
///
/// # Returns
/// The trimmed data, or `""` when there is none.
pub fn first_event_data(body: &str) -> &str {
    body.split("\n\n")
        .next()
        .and_then(|event| event.lines().find_map(|line| line.strip_prefix("data:")))
        .unwrap_or("")
        .trim()
}

/// Opens an MCP session with `key`.
///
/// # Arguments
/// * `mcp_url` - the replica's `/mcp` URL.
/// * `key` - the bearer key.
///
/// # Returns
/// The `Mcp-Session-Id` the server issued.
///
/// # Panics
/// If the server does not answer 200 with a session id.
pub async fn open_session(mcp_url: &str, key: &str) -> String {
    let response = reqwest::Client::new()
        .post(mcp_url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .bearer_auth(key)
        .body(INITIALIZE)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response
        .headers()
        .get("mcp-session-id")
        .expect("a session id")
        .to_str()
        .unwrap()
        .to_string()
}

/// POSTs `body` on `session` with `key`.
///
/// # Arguments
/// * `mcp_url` - the replica's `/mcp` URL.
/// * `key` - the bearer key.
/// * `session` - the `Mcp-Session-Id`.
/// * `body` - a JSON-RPC message.
///
/// # Returns
/// The response (its body unread).
///
/// # Panics
/// If the request cannot be sent.
pub async fn post_on(mcp_url: &str, key: &str, session: &str, body: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(mcp_url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-session-id", session)
        .header("mcp-protocol-version", "2025-11-25")
        .bearer_auth(key)
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

/// Posts `notifications/initialized` on `session` with `key`.
///
/// # Returns
/// The status: 202 when rmcp accepts it.
pub async fn notify_on(mcp_url: &str, key: &str, session: &str) -> StatusCode {
    post_on(mcp_url, key, session, INITIALIZED).await.status()
}

/// Opens `session`'s event stream (`GET /mcp`) with `key`.
///
/// # Returns
/// The status (the body, a stream, is never read).
pub async fn stream_on(mcp_url: &str, key: &str, session: &str) -> StatusCode {
    reqwest::Client::new()
        .get(mcp_url)
        .header("accept", "text/event-stream")
        .header("mcp-session-id", session)
        .header("mcp-protocol-version", "2025-11-25")
        .bearer_auth(key)
        .send()
        .await
        .unwrap()
        .status()
}

/// Ends `session` (`DELETE /mcp`) with `key`.
///
/// # Returns
/// The status: 202 when the session ended.
pub async fn delete_on(mcp_url: &str, key: &str, session: &str) -> StatusCode {
    reqwest::Client::new()
        .delete(mcp_url)
        .header("mcp-session-id", session)
        .header("mcp-protocol-version", "2025-11-25")
        .bearer_auth(key)
        .send()
        .await
        .unwrap()
        .status()
}

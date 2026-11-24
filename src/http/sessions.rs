//! MCP sessions bound to the key that opened them (M5 Decision 2).
//!
//! rmcp knows a session only by its `Mcp-Session-Id`, and each request is
//! authorized by its own key, so without this a second key that learned a
//! session id could post on the session or attach to its `GET /mcp` stream.
//! [`bind_session`] records the key of the request that opens a session and
//! answers any later request on it with another key, or on a session it
//! never bound, with rmcp's own 404 for an unknown session.
//!
//! With `[redis]` the bindings live in Redis beside rmcp's stored session
//! state ([`SessionBindings::Redis`], spec Decisions 9-11), so a session
//! opened on one replica is served, and guarded, on every other.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::app::App;
use crate::auth::Principal;
use crate::error::OkfError;
use crate::http::AppError;
use crate::selection::SESSION_HEADER;
use crate::shared::sessions::{unavailable, watch_restore, RedisSessions};
use crate::shared::Redis;

/// The body rmcp answers an unknown session with; a refused key gets the
/// same, so it learns nothing about the session.
const SESSION_NOT_FOUND: &str = "Not Found: Session not found";

/// One session's key, and when it was last used.
#[derive(Debug)]
struct Binding {
    key_id: String,
    seen: Instant,
}

/// Every live session's key, by session id.
#[derive(Debug)]
pub struct SessionKeys {
    idle: Duration,
    // A `std` mutex: held only for a map lookup or insert, never across an
    // `.await`, so it never blocks an async thread for long.
    bindings: Mutex<HashMap<String, Binding>>,
}

impl SessionKeys {
    /// An empty map whose bindings expire after `idle` without use.
    ///
    /// # Arguments
    /// * `idle` - `session_idle_timeout`, the same bound rmcp's sessions have.
    ///
    /// # Returns
    /// The map.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial constructor: builds a value from its arguments"
        )
    )]
    pub fn new(idle: Duration) -> Self {
        Self {
            idle,
            bindings: Mutex::new(HashMap::new()),
        }
    }

    /// Records that `key_id` opened `session`, first dropping every binding
    /// idle past the timeout (so the map holds live sessions only).
    ///
    /// # Arguments
    /// * `session` - the `Mcp-Session-Id` rmcp issued.
    /// * `key_id` - the key of the request that opened it.
    /// * `now` - the time of that request.
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn bind(&self, session: &str, key_id: &str, now: Instant) {
        let mut bindings = self.lock();
        // `retain` keeps the entries the closure accepts: Python's
        // `{k: v for k, v in d.items() if keep(v)}`, in place.
        bindings.retain(|_, binding| now.saturating_duration_since(binding.seen) <= self.idle);
        bindings.insert(
            session.to_string(),
            Binding {
                key_id: key_id.to_string(),
                seen: now,
            },
        );
    }

    /// May `key_id` use `session`? Using it restarts its idle clock.
    ///
    /// # Arguments
    /// * `session` - the request's `Mcp-Session-Id`.
    /// * `key_id` - the request's key.
    /// * `now` - the time of the request.
    ///
    /// # Returns
    /// `true` only if `key_id` opened the session and it has not idled out;
    /// an idled-out binding is dropped.
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn admits(&self, session: &str, key_id: &str, now: Instant) -> bool {
        let mut bindings = self.lock();
        let Some(binding) = bindings.get_mut(session) else {
            return false;
        };
        if now.saturating_duration_since(binding.seen) > self.idle {
            bindings.remove(session);
            return false;
        }
        if binding.key_id != key_id {
            return false;
        }
        binding.seen = now;
        true
    }

    /// Forgets a session (after its `DELETE /mcp`).
    ///
    /// # Arguments
    /// * `session` - its id.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn release(&self, session: &str) {
        self.lock().remove(session);
    }

    /// How many sessions are bound.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }

    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Binding>> {
        // Poisoned only if a thread panicked while holding it; nothing here
        // can panic with it held.
        self.bindings.lock().expect("session binding lock poisoned")
    }
}

/// Where the bindings live: this process (no `[redis]`; [`SessionKeys`],
/// unchanged) or Redis (spec Decision 10), shared by every replica.
#[derive(Debug)]
pub enum SessionBindings {
    /// One process's memory.
    Memory(SessionKeys),
    /// Redis. In an `Arc` because rmcp holds the same value as its
    /// `session_store` ([`SessionBindings::store`]).
    Redis(Arc<RedisSessions>),
}

impl SessionBindings {
    /// Bindings in Redis when `redis` is set, else in memory.
    ///
    /// # Arguments
    /// * `redis` - the shared Redis, if any.
    /// * `idle` - `session_idle_timeout`.
    ///
    /// # Returns
    /// The bindings.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial constructor: builds a value from its arguments"
        )
    )]
    pub fn new(redis: Option<&Redis>, idle: Duration) -> Self {
        match redis {
            None => Self::Memory(SessionKeys::new(idle)),
            Some(redis) => Self::Redis(Arc::new(RedisSessions::new(redis.clone(), idle))),
        }
    }

    /// The store rmcp persists sessions to, so another replica can restore
    /// them; `None` in memory.
    ///
    /// # Returns
    /// The Redis sessions, for `StreamableHttpServerConfig::session_store`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn store(&self) -> Option<Arc<RedisSessions>> {
        match self {
            Self::Memory(_) => None,
            Self::Redis(sessions) => Some(Arc::clone(sessions)),
        }
    }

    /// May `key_id` use `session`? Using it restarts its idle clock.
    ///
    /// # Arguments
    /// * `session` - the request's `Mcp-Session-Id`.
    /// * `key_id` - the request's key.
    /// * `now` - the time of the request (memory only).
    ///
    /// # Returns
    /// `true` only if `key_id` opened the session and it is still live.
    ///
    /// # Errors
    /// [`OkfError::Busy`] when Redis fails.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn admits(
        &self,
        session: &str,
        key_id: &str,
        now: Instant,
    ) -> Result<bool, OkfError> {
        match self {
            Self::Memory(keys) => Ok(keys.admits(session, key_id, now)),
            Self::Redis(sessions) => sessions.admits(session, key_id).await,
        }
    }

    /// Records that `key_id` opened `session`.
    ///
    /// # Arguments
    /// * `session` - the `Mcp-Session-Id` rmcp issued.
    /// * `key_id` - the key of the request that opened it.
    /// * `now` - the time of that request (memory only).
    ///
    /// # Errors
    /// [`OkfError::Busy`] when Redis fails or holds no state for the session
    /// (rmcp's `store` failed).
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn bind(&self, session: &str, key_id: &str, now: Instant) -> Result<(), OkfError> {
        match self {
            Self::Memory(keys) => {
                keys.bind(session, key_id, now);
                Ok(())
            }
            Self::Redis(sessions) => sessions.bind(session, key_id).await,
        }
    }

    /// Forgets a session (its `DELETE /mcp`).
    ///
    /// # Arguments
    /// * `session` - its id.
    ///
    /// # Errors
    /// [`OkfError::Busy`] when Redis fails.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn release(&self, session: &str) -> Result<(), OkfError> {
        match self {
            Self::Memory(keys) => {
                keys.release(session);
                Ok(())
            }
            Self::Redis(sessions) => sessions.release(session).await,
        }
    }
}

/// The `/mcp` middleware of M5 Decision 2. It runs after `require_key`, so
/// the request carries its [`Principal`].
///
/// - A request naming a session is refused with 404 unless that session is
///   bound to this request's key. This runs before rmcp, so a refused
///   request never reaches rmcp's store.
/// - A response that issues a session (`Mcp-Session-Id` on the answer to a
///   request that named none) binds it to the request's key.
/// - A `DELETE` that rmcp accepted releases the binding. rmcp accepts it
///   even on a replica that never held the session, so with Redis it ends
///   the session on every replica.
/// - With Redis, a Redis failure (during the check, the bind, the release or
///   rmcp's restore), or a session whose state rmcp failed to store, answers
///   503: nothing falls back to memory (spec Decision 16).
///
/// # Arguments
/// * `app` - the shared state ([`App::sessions`]).
/// * `request` - the request.
/// * `next` - the rest of the stack (rmcp's service).
///
/// # Returns
/// rmcp's response, 404, or 503.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn bind_session(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let key_id = request
        .extensions()
        .get::<Arc<Principal>>()
        .map(|principal| principal.key_id.clone());
    // Never logged: a session id lets its holder use the session.
    let session = request
        .headers()
        .get(SESSION_HEADER)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
    let now = Instant::now();
    let closing = request.method() == Method::DELETE;
    let shared = matches!(app.sessions(), SessionBindings::Redis(_));
    if let Some(session) = &session {
        let admitted = match &key_id {
            Some(key) => app.sessions().admits(session, key, now).await,
            None => Ok(false),
        };
        match admitted {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!("refused a request on another key's or an unbound MCP session");
                return (StatusCode::NOT_FOUND, SESSION_NOT_FOUND).into_response();
            }
            Err(error) => return AppError::from(error).into_response(),
        }
    }
    let response = if shared {
        // `watch_restore` runs rmcp with a request-scoped flag that
        // `RedisSessions::load` sets when Redis failed during a restore: rmcp
        // then answers 404, but the session may well exist, so the client
        // gets a retryable 503 instead (spec Decision 16). The flag carries
        // no session id.
        let (response, restore_failed) = watch_restore(next.run(request)).await;
        if restore_failed {
            return AppError::from(unavailable()).into_response();
        }
        response
    } else {
        next.run(request).await
    };
    match (&session, &key_id) {
        (None, Some(key)) => {
            if let Some(issued) = response.headers().get(SESSION_HEADER) {
                let issued = String::from_utf8_lossy(issued.as_bytes());
                if let Err(error) = app.sessions().bind(&issued, key, now).await {
                    // The unbound local session idles out; the client
                    // initializes again.
                    return AppError::from(error).into_response();
                }
            }
        }
        // Only after rmcp accepted the `DELETE`: a refused one (a disallowed
        // `Host`, say) must leave the session in place. rmcp answers 202 even
        // on a replica that holds no local copy, so in Redis mode this works
        // on any replica.
        (Some(session), _) if closing && response.status().is_success() => {
            if let Err(error) = app.sessions().release(session).await {
                // The keys stay; the client can repeat the `DELETE`.
                return AppError::from(error).into_response();
            }
        }
        // A request on a bound session (already refreshed by `admits`), or
        // one without a key (only a router without `require_key`).
        _ => {}
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Duration = Duration::from_secs(60);

    #[test]
    fn a_session_admits_only_the_key_that_opened_it() {
        let keys = SessionKeys::new(IDLE);
        let start = Instant::now();

        keys.bind("s1", "alice", start);

        assert!(keys.admits("s1", "alice", start + Duration::from_secs(1)));
        assert!(!keys.admits("s1", "bob", start + Duration::from_secs(1)));
        assert!(!keys.admits("unknown", "alice", start));
    }

    #[test]
    fn use_keeps_a_binding_and_idling_drops_it() {
        let keys = SessionKeys::new(IDLE);
        let start = Instant::now();
        keys.bind("busy", "alice", start);
        keys.bind("idle", "alice", start);

        // `busy` is used just inside the timeout, which restarts its clock.
        let used = keys.admits("busy", "alice", start + IDLE);
        let later = start + IDLE * 2;
        let busy_later = keys.admits("busy", "alice", later);
        let idle_later = keys.admits("idle", "alice", later);

        assert!(used);
        assert!(busy_later);
        assert!(!idle_later, "an idle binding admits no one");
    }

    #[test]
    fn binding_a_new_session_prunes_idle_ones_and_release_drops_one() {
        let keys = SessionKeys::new(IDLE);
        let start = Instant::now();
        keys.bind("old", "alice", start);
        keys.bind("closed", "alice", start + IDLE * 2);

        keys.bind("new", "bob", start + IDLE * 2);
        keys.release("closed");

        assert_eq!(keys.len(), 1);
        assert!(keys.admits("new", "bob", start + IDLE * 2));
    }

    /// The `initialize` body the tests send.
    const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;

    /// POSTs an `initialize` to `endpoint` with `key`.
    async fn initialize(endpoint: &str, key: &str) -> reqwest::Response {
        reqwest::Client::new()
            .post(endpoint)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .bearer_auth(key)
            .body(INITIALIZE)
            .send()
            .await
            .expect("the server answers")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_whose_state_write_failed_is_never_bound() {
        tokio::time::timeout(Duration::from_secs(120), async {
            use crate::store::keys::{self, Access, Grants, NewKey};

            let (_provider, metrics, registry) = crate::mcp::testing::local_metrics();
            let (test, _server) = crate::mcp::testing::app_on_redis_with(
                crate::config::Config::default(),
                crate::db::redis_support::TestRedis::start().await,
                &metrics,
            )
            .await;
            let key = keys::create(
                test.app.accounts().executor(),
                &NewKey {
                    name: "reader",
                    access: Access::Read,
                    maintenance_report: false,
                    grants: Grants::All,
                    default_catalog: None,
                    actor: "reader",
                    user: crate::db::TEST_USER,
                    self_issued: false,
                    expires_at: None,
                    levels: &[],
                },
                &crate::store::audit::AuditActor::cli(),
                chrono::Utc::now(),
            )
            .await
            .expect("create a key");
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let endpoint = format!("http://{}/mcp", listener.local_addr().expect("address"));
            let shutdown = tokio_util::sync::CancellationToken::new();
            let served = tokio::spawn(crate::http::serve_with(
                Arc::clone(&test.app),
                listener,
                shutdown.clone(),
                crate::http::ServeOptions::NO_WORKER,
            ));
            let store = test.app.sessions().store().expect("Redis bindings");
            store.fail_next_store();

            let refused = initialize(&endpoint, &key).await.status();
            let redis = test.app.redis().expect("the app's redis");
            let bound: Vec<String> = redis::cmd("KEYS")
                .arg(redis.key("mcp:bind", "*"))
                .query_async(&mut redis.connection())
                .await
                .expect("list the bindings");
            let rendered = crate::mcp::testing::render(&registry);
            let retried = initialize(&endpoint, &key).await.status();

            assert_eq!(refused, StatusCode::SERVICE_UNAVAILABLE);
            assert!(bound.is_empty(), "no binding without a state");
            assert_eq!(
                crate::shared::testing::error_count(&rendered, "bind"),
                1,
                "{rendered}"
            );
            assert_eq!(retried, StatusCode::OK, "the client initializes again");
            shutdown.cancel();
            let _ = served.await;
        })
        .await
        .expect("the test finishes within 120 s");
    }
}

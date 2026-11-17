//! Optional shared state for running several `serve` replicas (spec
//! `2026-11-17-okf-multi-replica-design.md`): one Redis connection and the
//! key names every component uses. Nothing here runs without `[redis]`.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use redis::aio::{ConnectionManager, ConnectionManagerConfig};

use crate::config::RedisConfig;
use crate::error::OkfError;
use crate::telemetry::metrics::{Metrics, RedisOp};
use crate::telemetry::subscriber::split_event;

/// How long a connection attempt may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long one command may take before it counts as failed.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);

/// The instance's Redis: a multiplexed connection that reconnects by itself,
/// the client (for the pub/sub connection), the key prefix and the metrics.
/// Cheap to clone (every field is a handle).
#[derive(Clone)]
pub struct Redis {
    client: redis::Client,
    connection: ConnectionManager,
    prefix: Arc<str>,
    metrics: Metrics,
}

// Hand-written: the client holds the URL, which may carry a password.
impl std::fmt::Debug for Redis {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "one-line accessor or key formatter: a span would record nothing"
        )
    )]
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Redis")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl Redis {
    /// Connects and checks the server answers, for `serve` start-up.
    ///
    /// # Arguments
    /// * `config` - the `[redis]` table (validated).
    /// * `env` - reads an environment variable (injectable for tests).
    /// * `metrics` - where `okf.redis.errors` is counted.
    ///
    /// # Returns
    /// The handle.
    ///
    /// # Errors
    /// [`OkfError::Config`] (path `[redis]`) when the variable is unset or
    /// blank, its value is not a Redis URL, or Redis does not answer `PING`
    /// within [`CONNECT_TIMEOUT`]. The message names the variable and the
    /// error's kind, never the URL.
    #[tracing::instrument(level = "info", skip_all, fields(db.system.name = "redis", db.operation.name = "connect"))]
    pub async fn connect(
        config: &RedisConfig,
        env: &dyn Fn(&str) -> Option<String>,
        metrics: &Metrics,
    ) -> Result<Self, OkfError> {
        // `rediss://` builds a rustls config from the process default; with
        // two providers compiled in, rustls would panic without this. `Err`
        // only means one is installed already.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let name = &config.url_env;
        let fail = |message: String| OkfError::Config {
            path: "[redis]".to_string(),
            message,
        };
        let url = env(name)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                fail(format!(
                    "redis.url_env names {name}, which is not set or is empty"
                ))
            })?;
        let client = redis::Client::open(url.as_str())
            .map_err(|_| fail(format!("{name} does not hold a redis:// or rediss:// URL")))?;
        let manager_config = ConnectionManagerConfig::new()
            .set_connection_timeout(Some(CONNECT_TIMEOUT))
            .set_response_timeout(Some(RESPONSE_TIMEOUT))
            .set_number_of_retries(1);
        let unreachable = |error: redis::RedisError| {
            metrics.redis_error(RedisOp::Connect);
            fail(format!(
                "the Redis {name} names did not answer ({:?})",
                error.kind()
            ))
        };
        let mut connection = ConnectionManager::new_with_config(client.clone(), manager_config)
            .await
            .map_err(unreachable)?;
        let _: String = redis::cmd("PING")
            .query_async(&mut connection)
            .await
            .map_err(unreachable)?;
        Ok(Self {
            client,
            connection,
            prefix: Arc::from(config.key_prefix.as_str()),
            metrics: metrics.clone(),
        })
    }

    /// The full key of `id` in `space` (`<prefix><space>:<id>`).
    ///
    /// # Arguments
    /// * `space` - the key space (`"oauth:code"`, ...).
    /// * `id` - already hashed where it came from a client ([`hashed`]).
    ///
    /// # Returns
    /// The key.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "one-line accessor or key formatter: a span would record nothing"
        )
    )]
    pub fn key(&self, space: &str, id: &str) -> String {
        format!("{}{space}:{id}", self.prefix)
    }

    /// The pub/sub channel `name` under the prefix.
    ///
    /// # Arguments
    /// * `name` - the channel's own name.
    ///
    /// # Returns
    /// The channel.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "one-line accessor or key formatter: a span would record nothing"
        )
    )]
    pub fn channel(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }

    /// A handle on the shared connection, for one call (a clone is cheap:
    /// every clone multiplexes the same socket).
    ///
    /// # Returns
    /// The connection.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "one-line accessor or key formatter: a span would record nothing"
        )
    )]
    pub fn connection(&self) -> ConnectionManager {
        self.connection.clone()
    }

    /// The client, for a dedicated pub/sub connection.
    ///
    /// # Returns
    /// The client.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "one-line accessor or key formatter: a span would record nothing"
        )
    )]
    pub fn client(&self) -> &redis::Client {
        &self.client
    }

    /// Counts and logs a failed call, and gives the error a request answers
    /// with: 503, retryable.
    ///
    /// # Arguments
    /// * `op` - what the call was doing.
    /// * `error` - the driver's error (its text stays local).
    ///
    /// # Returns
    /// [`OkfError::Busy`].
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn failed(&self, op: RedisOp, error: &redis::RedisError) -> OkfError {
        self.metrics.redis_error(op);
        split_event!(
            error,
            { operation = op.as_str(), kind = ?error.kind(), },
            { %error, },
            "a Redis call failed"
        );
        OkfError::Busy {
            reason: "the shared session store is unavailable".to_string(),
        }
    }

    /// Counts a failure that has no driver error (a pub/sub stream that
    /// ended).
    ///
    /// # Arguments
    /// * `op` - what was failing.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn count(&self, op: RedisOp) {
        self.metrics.redis_error(op);
    }

    /// Counts a value that would not decode (another version, or corrupt).
    ///
    /// # Arguments
    /// * `space` - its key space (a closed set, safe to log).
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn undecodable(&self, space: &'static str) {
        self.metrics.redis_error(RedisOp::Decode);
        tracing::warn!(space, "a stored value did not decode; treated as absent");
    }
}

/// The hex SHA-256 of a client-presented id, the only form an id takes in a
/// key name (spec Decision 7).
///
/// # Arguments
/// * `id` - the id as presented.
///
/// # Returns
/// 64 lowercase hex characters.
#[tracing::instrument(level = "debug", skip_all)]
pub fn hashed(id: &str) -> String {
    // `fold` with `write!` appends each byte's two hex digits to one
    // preallocated `String` (Python: `hashlib.sha256(...).hexdigest()`).
    crate::keys::hash(id)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            // Writing to a `String` cannot fail.
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// Helpers for the library's unit tests.
#[cfg(test)]
pub(crate) mod testing {
    use super::Redis;
    use crate::config::RedisConfig;
    use crate::db::redis_support::{TestRedis, URL_VAR};
    use crate::telemetry::metrics::Metrics;

    /// The `[redis]` table that points at `server`, with its random prefix.
    pub(crate) fn config(server: &TestRedis) -> RedisConfig {
        RedisConfig {
            url_env: URL_VAR.to_string(),
            key_prefix: server.prefix.clone(),
        }
    }

    /// Connects to `server` through [`Redis::connect`].
    pub(crate) async fn connect(server: &TestRedis) -> Redis {
        Redis::connect(&config(server), &server.env(), &Metrics::global())
            .await
            .expect("connect to the test redis")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_refuses_an_unset_or_blank_variable_and_names_only_it() {
        let config = RedisConfig {
            url_env: "OKF_TEST_REDIS_URL".into(),
            key_prefix: "okf:".into(),
        };
        for value in [None, Some("  ")] {
            let error = Redis::connect(&config, &|_| value.map(str::to_string), &Metrics::global())
                .await
                .unwrap_err();
            let message = error.to_string();
            assert!(message.contains("OKF_TEST_REDIS_URL"), "{message}");
        }
    }

    #[tokio::test]
    async fn connect_refuses_an_unreachable_or_malformed_url_without_printing_it() {
        let config = RedisConfig {
            url_env: "OKF_TEST_REDIS_URL".into(),
            key_prefix: "okf:".into(),
        };
        for url in [
            "redis://:s3cretpw@127.0.0.1:1/0",
            "http://user:s3cretpw@example.com",
        ] {
            let error = Redis::connect(&config, &|_| Some(url.to_string()), &Metrics::global())
                .await
                .unwrap_err();
            let message = error.to_string();
            assert!(
                !message.contains("s3cretpw")
                    && !message.contains("127.0.0.1")
                    && !message.contains("example.com"),
                "{message}"
            );
            assert!(message.contains("OKF_TEST_REDIS_URL"), "{message}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connect_answers_a_live_redis() {
        let server = crate::db::redis_support::TestRedis::start().await;
        let redis = tokio::time::timeout(Duration::from_secs(60), testing::connect(&server))
            .await
            .expect("connecting to the test redis within 60 s");
        assert!(redis.key("oauth:code", "abc").ends_with("oauth:code:abc"));
        assert!(redis.channel("announce").ends_with("announce"));
        assert!(!format!("{redis:?}").contains(&server.url));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_replicas_share_one_redis_and_a_single_app_has_one() {
        let (first, second, _redis) = tokio::time::timeout(
            Duration::from_secs(120),
            crate::mcp::testing::replica_pair(&[], crate::config::Config::default()),
        )
        .await
        .expect("starting two replicas within 120 s");
        let (one, two) = (
            first.app.redis().expect("the first app's redis"),
            second.redis().expect("the second app's redis"),
        );
        let key = one.key("test", "shared");
        assert_eq!(key, two.key("test", "shared"));
        let mut connection = one.connection();
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg("42")
            .query_async(&mut connection)
            .await
            .expect("set through the first replica");
        let mut other = two.connection();
        let read: Option<String> = redis::cmd("GET")
            .arg(&key)
            .query_async(&mut other)
            .await
            .expect("get through the second replica");
        assert_eq!(read.as_deref(), Some("42"));
        assert_eq!(first.app.config().poll_interval, Duration::from_secs(3600));

        let (single, _redis) =
            crate::mcp::testing::app_with_redis(crate::config::Config::default()).await;
        assert!(single.app.redis().is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn app_load_refuses_a_redis_table_whose_variable_is_unset() {
        let db = crate::db::test_db().await;
        let config = crate::config::Config {
            redis: Some(RedisConfig {
                url_env: "OKF_TEST_REDIS_URL_NEVER_SET".into(),
                key_prefix: "okf:".into(),
            }),
            ..crate::config::Config::default()
        };
        let error =
            crate::app::App::load(db.pool.clone(), config, Arc::new(crate::clock::SystemClock))
                .await
                .map(|_| ())
                .unwrap_err();
        let message = error.to_string();
        assert!(message.starts_with("[redis]:"), "{message}");
        assert!(
            message.contains("OKF_TEST_REDIS_URL_NEVER_SET"),
            "{message}"
        );
    }

    #[test]
    fn hashed_is_the_hex_sha256() {
        assert_eq!(
            hashed("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

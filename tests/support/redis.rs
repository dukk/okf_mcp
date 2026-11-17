//! A Redis for tests: the shared server `TEST_REDIS_URL` names, else a
//! `redis:7.4-alpine` container per [`TestRedis`] (testcontainers-modules'
//! default tag, 5.0, lacks GETDEL). Each [`TestRedis`] has its own random key
//! prefix, so tests sharing one server never see each other's keys.

// Each crate that includes this file uses a different part of it.
#![allow(dead_code)]

use testcontainers_modules::redis::{Redis as RedisImage, REDIS_PORT};
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

/// The variable naming a running Redis every test may share.
pub const SERVER_URL_VAR: &str = "TEST_REDIS_URL";
/// The variable a test's `[redis] url_env` names; [`TestRedis::env`] answers it.
pub const URL_VAR: &str = "OKF_TEST_REDIS_URL";

/// One test's Redis: its URL, its key prefix, and the container if it
/// started one (removed when this is dropped).
pub struct TestRedis {
    /// `redis://127.0.0.1:<port>/0`.
    pub url: String,
    /// `okftest:<pid>-<n>:`, unique to this value.
    pub prefix: String,
    container: Option<ContainerAsync<RedisImage>>,
}

impl TestRedis {
    /// Uses `TEST_REDIS_URL`, or starts a labelled container, retried once
    /// (`pg::start_with_retry`).
    ///
    /// # Panics
    /// If Docker cannot start the container.
    pub async fn start() -> Self {
        let prefix = format!("okftest:{}:", super::pg::unique_start_label());
        if let Some(url) = std::env::var(SERVER_URL_VAR)
            .ok()
            .filter(|url| !url.is_empty())
        {
            return Self {
                url,
                prefix,
                container: None,
            };
        }
        let container =
            super::pg::start_with_retry(|| RedisImage::default().with_tag("7.4-alpine")).await;
        let port = container
            .get_host_port_ipv4(REDIS_PORT)
            .await
            .expect("the redis port");
        Self {
            url: format!("redis://127.0.0.1:{port}/0"),
            prefix,
            container: Some(container),
        }
    }

    /// An `env` reader for `Redis::connect` that knows only [`URL_VAR`].
    pub fn env(&self) -> impl Fn(&str) -> Option<String> + '_ {
        move |name| (name == URL_VAR).then(|| self.url.clone())
    }

    /// Stops the container (outage tests); `None` on a shared server, where
    /// a test cannot stop Redis for everyone and should skip itself.
    ///
    /// # Panics
    /// If Docker refuses the stop.
    pub async fn stop(&self) -> Option<()> {
        self.container
            .as_ref()?
            .stop()
            .await
            .expect("stop the redis container");
        Some(())
    }

    /// Starts a stopped container again on the same host port.
    ///
    /// # Panics
    /// If Docker refuses the start.
    pub async fn start_again(&self) -> Option<()> {
        self.container
            .as_ref()?
            .start()
            .await
            .expect("start the redis container");
        Some(())
    }
}

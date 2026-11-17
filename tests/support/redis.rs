//! A Redis for tests: the shared server `TEST_REDIS_URL` names, else a
//! `redis:7.4-alpine` container per [`TestRedis`] (testcontainers-modules'
//! default tag, 5.0, lacks GETDEL). [`TestRedis::start_cluster`] does the same
//! for a one-node Valkey cluster (`TEST_REDIS_CLUSTER_URL`, else a
//! `valkey/valkey:8.1-alpine` container). Each [`TestRedis`] has its own random
//! key prefix, so tests sharing one server never see each other's keys.

// Each crate that includes this file uses a different part of it.
#![allow(dead_code)]

use std::time::Duration;

use testcontainers_modules::redis::{Redis as RedisImage, REDIS_PORT};
use testcontainers_modules::testcontainers::core::{ContainerPort, WaitFor};
use testcontainers_modules::testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// The variable naming a running Redis every test may share.
pub const SERVER_URL_VAR: &str = "TEST_REDIS_URL";
/// The variable naming a running one-node (or larger) Redis cluster.
pub const CLUSTER_URL_VAR: &str = "TEST_REDIS_CLUSTER_URL";
/// The variable a test's `[redis] url_env` names; [`TestRedis::env`] answers it.
pub const URL_VAR: &str = "OKF_TEST_REDIS_URL";

/// A key prefix no other [`TestRedis`] shares, even on a persistent
/// `TEST_REDIS_URL` server: `okftest:<pid>-<n>-<16 random hex digits>:`. The
/// pid and counter only help a person reading the keys; the random part is
/// what keeps runs in separate containers, or after a pid is reused, apart.
///
/// # Panics
/// If the operating system has no randomness: a broken test machine.
pub fn random_prefix() -> String {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).expect("random bytes for the test key prefix");
    let random: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("okftest:{}-{random}:", super::pg::unique_start_label())
}

/// One test's Redis: its URL, its key prefix, and the container if it
/// started one (removed when this is dropped).
pub struct TestRedis {
    /// `redis://127.0.0.1:<port>/0`.
    pub url: String,
    /// [`random_prefix`], unique to this value.
    pub prefix: String,
    /// Whether the server is a cluster: the test's `[redis] mode` follows it.
    pub cluster: bool,
    container: Option<ContainerAsync<RedisImage>>,
    cluster_container: Option<ContainerAsync<GenericImage>>,
}

impl TestRedis {
    /// Uses `TEST_REDIS_URL`, or starts a labelled container, retried once
    /// (`pg::start_with_retry`).
    ///
    /// # Panics
    /// If Docker cannot start the container.
    pub async fn start() -> Self {
        let prefix = random_prefix();
        if let Some(url) = std::env::var(SERVER_URL_VAR)
            .ok()
            .filter(|url| !url.is_empty())
        {
            return Self {
                url,
                prefix,
                cluster: false,
                container: None,
                cluster_container: None,
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
            cluster: false,
            container: Some(container),
            cluster_container: None,
        }
    }

    /// Uses `TEST_REDIS_CLUSTER_URL`, or starts a one-node Valkey cluster:
    /// `--cluster-enabled yes`, then `cluster-announce-ip` and
    /// `cluster-announce-port` set to the mapped host port (so a cluster
    /// client on the host follows the topology back to it), all 16384 slots
    /// added, and a bounded wait for `cluster_state:ok`.
    ///
    /// # Panics
    /// If Docker cannot start the container or the cluster does not form
    /// within 30 s.
    pub async fn start_cluster() -> Self {
        let prefix = random_prefix();
        if let Some(url) = std::env::var(CLUSTER_URL_VAR)
            .ok()
            .filter(|url| !url.is_empty())
        {
            return Self {
                url,
                prefix,
                cluster: true,
                container: None,
                cluster_container: None,
            };
        }
        let container = super::pg::start_with_retry(|| {
            GenericImage::new("valkey/valkey", "8.1-alpine")
                .with_exposed_port(ContainerPort::Tcp(REDIS_PORT))
                .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
                .with_cmd([
                    "valkey-server",
                    "--cluster-enabled",
                    "yes",
                    "--cluster-config-file",
                    "nodes.conf",
                    "--appendonly",
                    "no",
                    "--save",
                    "",
                ])
        })
        .await;
        let port = container
            .get_host_port_ipv4(REDIS_PORT)
            .await
            .expect("the valkey port");
        let url = format!("redis://127.0.0.1:{port}/0");
        tokio::time::timeout(Duration::from_secs(30), form_cluster(&url, port))
            .await
            .expect("the one-node cluster forms within 30 s");
        Self {
            url,
            prefix,
            cluster: true,
            container: None,
            cluster_container: Some(container),
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
        match (&self.container, &self.cluster_container) {
            (Some(container), _) => container.stop().await,
            (None, Some(container)) => container.stop().await,
            (None, None) => return None,
        }
        .expect("stop the redis container");
        Some(())
    }

    /// Starts a stopped container again on the same host port.
    ///
    /// # Panics
    /// If Docker refuses the start.
    pub async fn start_again(&self) -> Option<()> {
        match (&self.container, &self.cluster_container) {
            (Some(container), _) => container.start().await,
            (None, Some(container)) => container.start().await,
            (None, None) => return None,
        }
        .expect("start the redis container");
        Some(())
    }
}

/// Turns the fresh cluster-enabled node at `url` into a one-node cluster
/// owning every slot, announcing `127.0.0.1:<port>`; returns when
/// `CLUSTER INFO` says `cluster_state:ok`.
///
/// # Panics
/// On any command failure (the caller bounds the wait).
async fn form_cluster(url: &str, port: u16) {
    let client = redis::Client::open(url).expect("a valkey URL");
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .expect("connect to the valkey node");
    for command in [
        vec!["CONFIG", "SET", "cluster-announce-ip", "127.0.0.1"],
        vec!["CONFIG", "SET", "cluster-announce-port", &port.to_string()],
        vec!["CLUSTER", "ADDSLOTSRANGE", "0", "16383"],
    ] {
        let mut cmd = redis::cmd(command[0]);
        for argument in &command[1..] {
            cmd.arg(*argument);
        }
        let _: String = cmd
            .query_async(&mut connection)
            .await
            .expect("configure the one-node cluster");
    }
    loop {
        let info: String = redis::cmd("CLUSTER")
            .arg("INFO")
            .query_async(&mut connection)
            .await
            .expect("read the cluster state");
        if info.contains("cluster_state:ok") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_prefixes_have_the_documented_shape_and_never_repeat() {
        let first = random_prefix();
        let second = random_prefix();
        assert_ne!(first, second);
        for prefix in [&first, &second] {
            let body = prefix
                .strip_prefix("okftest:")
                .and_then(|rest| rest.strip_suffix(':'))
                .expect("okftest:...: shape");
            let random = body.rsplit('-').next().expect("a random part");
            assert_eq!(random.len(), 16, "{prefix}");
            assert!(random.bytes().all(|b| b.is_ascii_hexdigit()), "{prefix}");
        }
        // The random part differs even when the pid and counter could not.
        let tail = |prefix: &str| prefix.rsplit('-').next().map(str::to_string);
        assert_ne!(tail(&first), tail(&second));
    }
}

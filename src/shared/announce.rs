//! `admin_generation` announcements (spec Decision 12): the instance whose
//! reload saw a bump publishes it; every instance's subscriber reloads.
//!
//! Messages are not stored: one sent while an instance is not subscribed is
//! lost. So the subscriber reloads once after every (re)subscribe, and the
//! poll loop stays as the backstop.

use std::sync::Arc;
use std::time::Duration;

use redis::AsyncCommands;
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::config::Config;
use crate::shared::{Redis, SubscriptionEvent};
use crate::telemetry::metrics::{Metrics, RedisOp};

/// The channel's name under the prefix.
pub const CHANNEL: &str = "admin-generation";
/// The first wait before resubscribing; doubles up to [`BACKOFF_MAX`].
const BACKOFF_MIN: Duration = Duration::from_secs(1);
/// The longest wait between resubscribe attempts.
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// How long a subscription may stay disconnected before it is replaced. The
/// driver resubscribes by itself but gives up after a few failed attempts (a
/// Redis that stays down longer), and then no `Subscribed` would ever follow.
const RESUBSCRIBE_GRACE: Duration = Duration::from_secs(10);

/// Publishes a generation (or, from the CLI, an empty "check now"). A failure
/// is counted and logged; the poll loop covers a lost announcement.
///
/// # Arguments
/// * `redis` - the shared Redis.
/// * `generation` - the generation now loaded, or `None` for "check now".
#[tracing::instrument(
    level = "debug",
    skip_all,
    fields(db.system.name = "redis", db.operation.name = "publish")
)]
pub async fn publish(redis: &Redis, generation: Option<i64>) {
    let payload = generation
        .map(|value| value.to_string())
        .unwrap_or_default();
    let published: redis::RedisResult<usize> = redis
        .connection()
        .publish(redis.channel(CHANNEL), payload)
        .await;
    if let Err(error) = published {
        // `failed` counts and logs; its 503 is for requests, not for here.
        let _ = redis.failed(RedisOp::Publish, &error);
    }
}

/// After a CLI command that may have changed keys, users, teams or grants,
/// tells every serving instance to check `admin_generation` (spec Decision
/// 13). Best effort: the change is committed whatever happens here, so a
/// missing variable or an unreachable Redis is a `warn`, never a failure.
/// Nothing printed names the URL.
///
/// # Arguments
/// * `config` - the loaded configuration; without `[redis]` nothing happens.
/// * `env` - reads an environment variable (injectable for tests).
/// * `metrics` - where `okf.redis.errors` is counted.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn announce_from_cli(
    config: &Config,
    env: &dyn Fn(&str) -> Option<String>,
    metrics: &Metrics,
) {
    let Some(table) = &config.redis else { return };
    match Redis::connect(table, env, metrics).await {
        Ok(redis) => publish(&redis, None).await,
        // The error's text names the variable and a kind, never the URL, but
        // only the kind is logged: the rest adds nothing a log reader needs.
        Err(error) => tracing::warn!(
            kind = error.kind(),
            "announcement skipped; serving instances notice the change within poll_interval"
        ),
    }
}

/// The generation an announcement names, `None` for an empty or unreadable
/// payload (the CLI's "check now").
///
/// # Arguments
/// * `payload` - the message's bytes.
///
/// # Returns
/// The announced generation, if the payload holds one.
#[tracing::instrument(level = "trace", skip_all)]
fn announced_generation(payload: &[u8]) -> Option<i64> {
    std::str::from_utf8(payload)
        .ok()
        .and_then(|text| text.parse::<i64>().ok())
}

/// Runs until `shutdown`: subscribes, reloads once (messages sent while it was
/// not subscribed are lost), then reloads on every announcement of another
/// generation. Reconnects with backoff when the connection ends. Returns at
/// once without `[redis]`.
///
/// # Arguments
/// * `app` - the instance whose key cache is reloaded.
/// * `shutdown` - cancelled when the server stops.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "long-lived loop: a span would stay open until shutdown"
    )
)]
pub async fn listen(app: Arc<App>, shutdown: CancellationToken) {
    let Some(redis) = app.redis().cloned() else {
        return;
    };
    let source = RedisSource(redis.clone());
    run(&app, &redis, &source, RESUBSCRIBE_GRACE, &shutdown).await;
}

/// Where the subscriber's events come from: a live [`crate::shared::Subscription`]
/// in production, a script in tests.
#[async_trait::async_trait]
trait Events: Send {
    /// The next event, or `None` once the stream is gone for good.
    async fn next(&mut self) -> Option<SubscriptionEvent>;
}

/// Opens subscriptions; injectable so a test can script outages.
#[async_trait::async_trait]
trait Source: Send + Sync {
    /// The stream type one subscription yields.
    type Stream: Events;

    /// Subscribes to the announcement channel.
    async fn subscribe(&self) -> redis::RedisResult<Self::Stream>;
}

/// The real source: a subscription on the shared Redis.
struct RedisSource(Redis);

#[async_trait::async_trait]
impl Events for crate::shared::Subscription {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "long-lived loop or thin delegate: a span would record nothing"
        )
    )]
    async fn next(&mut self) -> Option<SubscriptionEvent> {
        crate::shared::Subscription::next(self).await
    }
}

#[async_trait::async_trait]
impl Source for RedisSource {
    type Stream = crate::shared::Subscription;

    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "long-lived loop or thin delegate: a span would record nothing"
        )
    )]
    async fn subscribe(&self) -> redis::RedisResult<Self::Stream> {
        self.0.subscribe(&self.0.channel(CHANNEL)).await
    }
}

/// The subscriber's loop, with its subscription source and grace period
/// injected: subscribes, reloads on `Subscribed` and on an announcement of
/// another generation, replaces a subscription that stays `Disconnected` for
/// `grace`, and subscribes again with backoff after the stream ends or a
/// subscribe fails.
///
/// # Arguments
/// * `app` - the instance whose key cache is reloaded.
/// * `redis` - where failures are counted.
/// * `source` - opens each subscription.
/// * `grace` - how long a subscription may stay disconnected.
/// * `shutdown` - cancelled when the server stops.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "long-lived loop: a span would stay open until shutdown"
    )
)]
async fn run<S: Source>(
    app: &App,
    redis: &Redis,
    source: &S,
    grace: Duration,
    shutdown: &CancellationToken,
) {
    let mut backoff = BACKOFF_MIN;
    while !shutdown.is_cancelled() {
        match source.subscribe().await {
            Ok(mut subscription) => {
                // When the library reported the connection lost, until it
                // reports the subscription again.
                let mut disconnected_at: Option<tokio::time::Instant> = None;
                loop {
                    // Never ready while connected; `pending` waits forever.
                    let replace_at = async {
                        match disconnected_at {
                            Some(since) => tokio::time::sleep_until(since + grace).await,
                            None => std::future::pending().await,
                        }
                    };
                    let event = tokio::select! {
                        () = shutdown.cancelled() => return,
                        // Drop the subscription; the code below backs off and
                        // subscribes again.
                        () = replace_at => None,
                        event = subscription.next() => event,
                    };
                    match event {
                        // The first subscribe and every automatic resubscribe:
                        // messages sent before it may have been lost, so reload.
                        Some(SubscriptionEvent::Subscribed) => {
                            backoff = BACKOFF_MIN;
                            disconnected_at = None;
                            app.reload_keys_now(false).await;
                        }
                        Some(SubscriptionEvent::Message(payload)) => {
                            if announced_generation(&payload) != Some(app.loaded_admin_generation())
                            {
                                app.reload_keys_now(false).await;
                            }
                        }
                        // The library resubscribes by itself; a `Subscribed`
                        // follows, or the grace above replaces the subscription.
                        Some(SubscriptionEvent::Disconnected) => {
                            redis.count(RedisOp::Subscribe);
                            disconnected_at.get_or_insert_with(tokio::time::Instant::now);
                        }
                        None => {
                            redis.count(RedisOp::Subscribe);
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                let _ = redis.failed(RedisOp::Subscribe, &error);
            }
        }
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::announced_generation;

    #[test]
    fn a_payload_names_a_generation_only_when_it_is_a_number() {
        assert_eq!(announced_generation(b"42"), Some(42));
        assert_eq!(announced_generation(b""), None);
        assert_eq!(announced_generation(b"not a number"), None);
        assert_eq!(announced_generation(&[0xff, 0xfe]), None);
    }
}

#[cfg(test)]
mod catch_up_tests {
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;

    use super::listen;
    use super::{run, Events, Source};
    use crate::config::Config;
    use crate::mcp::testing::replica_pair;
    use crate::shared::SubscriptionEvent;
    use crate::store::audit::AuditActor;

    /// A scripted subscription: yields its events, then waits forever.
    struct Scripted(std::collections::VecDeque<SubscriptionEvent>);

    #[async_trait::async_trait]
    impl Events for Scripted {
        #[cfg_attr(
            dylint_lib = "hyp_tracing",
            allow(uninstrumented_fn, reason = "test double")
        )]
        async fn next(&mut self) -> Option<SubscriptionEvent> {
            match self.0.pop_front() {
                Some(event) => Some(event),
                None => std::future::pending().await,
            }
        }
    }

    /// Attempt 1: `Disconnected` and nothing more. Attempt 2: fails.
    /// Attempt 3: `Subscribed`. Records whether the key was still cached when
    /// each attempt began.
    struct Outage {
        attempts: std::sync::Mutex<Vec<bool>>,
        holds_key: Box<dyn Fn() -> bool + Send + Sync>,
    }

    #[async_trait::async_trait]
    impl Source for Outage {
        type Stream = Scripted;

        #[cfg_attr(
            dylint_lib = "hyp_tracing",
            allow(uninstrumented_fn, reason = "test double")
        )]
        async fn subscribe(&self) -> redis::RedisResult<Scripted> {
            let attempt = {
                let mut attempts = self.attempts.lock().expect("attempts lock");
                attempts.push((self.holds_key)());
                attempts.len()
            };
            match attempt {
                1 => Ok(Scripted([SubscriptionEvent::Disconnected].into())),
                2 => Err(redis::RedisError::from((redis::ErrorKind::Io, "refused"))),
                _ => Ok(Scripted([SubscriptionEvent::Subscribed].into())),
            }
        }
    }

    /// A subscription that is `Disconnected` and never `Subscribed` again is
    /// replaced after the grace period; a subscribe that fails is retried
    /// after the backoff; the first `Subscribed` then reloads. Without the
    /// replacement attempt 2 never happens and B never reloads.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_subscription_stuck_disconnected_is_replaced_and_a_failed_subscribe_retried() {
        let (first, second, _redis) = tokio::time::timeout(
            Duration::from_secs(120),
            replica_pair(&[], Config::default()),
        )
        .await
        .expect("starting two replicas within 120 s");
        let a = first.app.clone();
        let b = second;
        let key = crate::mcp::testing::staff_root(&first).await;
        a.poll().await.expect("A's poll");
        b.reload_keys_now(false).await;
        let id = a.keys().find(&key).expect("cached").key_id.clone();
        crate::store::keys::revoke_by_id(a.pool(), &id, &AuditActor::cli(), chrono::Utc::now())
            .await
            .expect("revoke");
        a.poll().await.expect("A's poll");

        let holder = b.clone();
        let source = Outage {
            attempts: std::sync::Mutex::new(Vec::new()),
            holds_key: Box::new(move || holder.keys().find(&key).is_some()),
        };
        let shutdown = CancellationToken::new();
        let redis = b.redis().expect("B's redis").clone();
        let finished = tokio::time::timeout(Duration::from_secs(20), async {
            let looped = run(&b, &redis, &source, Duration::from_millis(200), &shutdown);
            tokio::pin!(looped);
            loop {
                tokio::select! {
                    () = &mut looped => return,
                    () = tokio::time::sleep(Duration::from_millis(50)) => {
                        let attempts = source.attempts.lock().expect("attempts lock").len();
                        if attempts >= 3 && !(source.holds_key)() {
                            shutdown.cancel();
                        }
                    }
                }
            }
        })
        .await;
        assert!(finished.is_ok(), "attempt 3 reloaded B within 20 s");
        let attempts = source.attempts.lock().expect("attempts lock").clone();
        assert_eq!(
            attempts,
            vec![true, true, true],
            "no reload before attempt 3"
        );
    }

    /// A change committed and announced while a replica has no subscription
    /// is not replayed by Redis: the replica's first `Subscribed` must reload.
    /// B's subscriber starts only after the announcement, so the message is
    /// lost for certain, and no other publish or poll follows.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_subscriber_reloads_when_it_subscribes_after_a_missed_announcement() {
        let (first, second, _redis) = tokio::time::timeout(
            Duration::from_secs(120),
            replica_pair(&[], Config::default()),
        )
        .await
        .expect("starting two replicas within 120 s");
        let a = first.app.clone();
        let b = second;
        let key = crate::mcp::testing::staff_root(&first).await;
        a.poll().await.expect("A's poll");
        b.reload_keys_now(false).await;
        assert!(b.keys().find(&key).is_some(), "B holds the key");

        let id = a.keys().find(&key).expect("cached").key_id.clone();
        crate::store::keys::revoke_by_id(a.pool(), &id, &AuditActor::cli(), chrono::Utc::now())
            .await
            .expect("revoke");
        // Announced to nobody: B has no subscription yet.
        a.poll().await.expect("A's poll");
        assert!(b.keys().find(&key).is_some(), "B has not heard of it");

        let shutdown = CancellationToken::new();
        let listener = tokio::spawn(listen(b.clone(), shutdown.clone()));
        let waited = tokio::time::timeout(Duration::from_secs(10), async {
            while b.keys().find(&key).is_some() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(10), listener)
            .await
            .expect("the subscriber stops")
            .expect("the subscriber's task");
        assert!(waited.is_ok(), "B reloaded when it subscribed");
    }
}

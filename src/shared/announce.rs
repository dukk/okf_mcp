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
use crate::shared::{Redis, SubscriptionEvent};
use crate::telemetry::metrics::RedisOp;

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
    let mut backoff = BACKOFF_MIN;
    while !shutdown.is_cancelled() {
        match redis.subscribe(&redis.channel(CHANNEL)).await {
            Ok(mut subscription) => {
                // When the library reported the connection lost, until it
                // reports the subscription again.
                let mut disconnected_at: Option<tokio::time::Instant> = None;
                loop {
                    // Never ready while connected; `pending` waits forever.
                    let replace_at = async {
                        match disconnected_at {
                            Some(since) => {
                                tokio::time::sleep_until(since + RESUBSCRIBE_GRACE).await;
                            }
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
    use crate::config::Config;
    use crate::mcp::testing::replica_pair;
    use crate::store::audit::AuditActor;

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

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
            Ok(mut subscription) => loop {
                let event = tokio::select! {
                    () = shutdown.cancelled() => return,
                    event = subscription.next() => event,
                };
                match event {
                    // The first subscribe and every automatic resubscribe:
                    // messages sent before it may have been lost, so reload.
                    Some(SubscriptionEvent::Subscribed) => {
                        backoff = BACKOFF_MIN;
                        app.reload_keys_now(false).await;
                    }
                    Some(SubscriptionEvent::Message(payload)) => {
                        if announced_generation(&payload) != Some(app.loaded_admin_generation()) {
                            app.reload_keys_now(false).await;
                        }
                    }
                    // The library resubscribes by itself; a `Subscribed` follows.
                    Some(SubscriptionEvent::Disconnected) => redis.count(RedisOp::Subscribe),
                    None => {
                        redis.count(RedisOp::Subscribe);
                        break;
                    }
                }
            },
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

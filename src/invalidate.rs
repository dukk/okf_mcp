//! The change feed (spec Decisions 10 and 12). A store function that
//! changes what a replica caches calls [`invalidate`] inside its own
//! transaction; Postgres delivers the `NOTIFY` to every listening replica
//! when, and only if, the transaction commits. [`listen`] runs on every
//! replica and applies each message to that replica's caches.
//!
//! A payload carries ids and generations only: never a key hash, a person's
//! name, an email, a label or catalog content.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sqlx::postgres::{PgListener, PgPoolOptions};
use sqlx::{PgConnection, PgPool};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::app::App;
use crate::backoff::Backoff;
use crate::error::OkfError;
use crate::store::catalogs;
use crate::telemetry::metrics::Metrics;
use crate::telemetry::subscriber::split_event;

/// The `LISTEN` channel.
pub const CHANNEL: &str = "okf_invalidate";

/// The payload version this build writes and reads.
const VERSION: u8 = 1;

/// The first and longest wait before the listener connects again.
const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// What changed. Ids only: never a key hash, a person's name or content.
// `serde(tag = "kind")` writes the variant as a `"kind"` field next to its
// own fields, e.g. `{"kind":"catalog","id":1,"generation":2}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Invalidation {
    /// A publish, import or repair moved the catalog to `generation`.
    Catalog {
        /// `catalogs.id`.
        id: i64,
        /// The generation the change reached.
        generation: i64,
    },
    /// The catalog row or its access facts changed (create, delete,
    /// details, owner, maintenance, member access, suspension). `name`
    /// (the catalog's slug, an id, not a person's data) lets a replica drop
    /// a cached "no such catalog" entry on create: the id is new, so no
    /// cache can map it back to the name.
    CatalogMeta {
        /// `catalogs.id`.
        id: i64,
        /// The catalog's name.
        name: String,
    },
    /// A key or grant (`api_keys.id`) changed.
    Credential {
        /// `api_keys.id`.
        key_id: String,
    },
    /// A user (`users.id`) changed: profile, admin flag, entitlements,
    /// unlocks, or anything that moves their ceiling.
    User {
        /// `users.id`.
        id: String,
    },
    /// A team (`teams.id`, text) changed: members, roles, label.
    Team {
        /// `teams.id`.
        id: String,
    },
    /// A server setting changed.
    Settings,
}

/// One received message: what changed, and when it was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// What changed.
    pub invalidation: Invalidation,
    /// The sender's clock at `invalidate`, in Unix milliseconds, for
    /// `okf.invalidations.lag`.
    pub sent_at_ms: i64,
}

// The wire form: the version and send time beside the flattened variant.
#[derive(serde::Serialize, serde::Deserialize)]
struct Wire {
    v: u8,
    at: i64,
    #[serde(flatten)]
    invalidation: Invalidation,
}

impl Invalidation {
    /// This invalidation's `NOTIFY` payload.
    ///
    /// # Arguments
    /// * `sent_at_ms` - the sender's clock, Unix milliseconds.
    ///
    /// # Returns
    /// The JSON payload.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn to_payload(&self, sent_at_ms: i64) -> String {
        serde_json::to_string(&Wire {
            v: VERSION,
            at: sent_at_ms,
            invalidation: self.clone(),
        })
        // A struct of integers and strings always serializes.
        .expect("an Invalidation always serializes")
    }

    /// The serde tag (`"catalog"`, ...), for metric labels and span fields.
    ///
    /// # Returns
    /// A fixed string per variant: never an id.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Catalog { .. } => "catalog",
            Self::CatalogMeta { .. } => "catalog_meta",
            Self::Credential { .. } => "credential",
            Self::User { .. } => "user",
            Self::Team { .. } => "team",
            Self::Settings => "settings",
        }
    }
}

impl Message {
    /// Parses a payload.
    ///
    /// # Arguments
    /// * `payload` - the `NOTIFY` payload.
    ///
    /// # Returns
    /// The message; `None` for another version, an unknown kind or anything
    /// else this build cannot read.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn parse(payload: &str) -> Option<Self> {
        let wire: Wire = serde_json::from_str(payload).ok()?;
        // `then_some`: `Some(..)` when the version matches, else `None`.
        (wire.v == VERSION).then_some(Self {
            invalidation: wire.invalidation,
            sent_at_ms: wire.at,
        })
    }

    /// How long ago the sender sent this, by this replica's clock.
    ///
    /// # Arguments
    /// * `now_ms` - this replica's clock, Unix milliseconds.
    ///
    /// # Returns
    /// The lag; zero when the clocks disagree (a negative lag).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn lag(&self, now_ms: i64) -> Duration {
        // `try_from` fails for a negative number: skew records no lag.
        Duration::from_millis(u64::try_from(now_ms - self.sent_at_ms).unwrap_or(0))
    }
}

/// Records `invalidations` in the caller's transaction: bumps
/// `admin_generation` once and sends one `NOTIFY` per invalidation, which
/// Postgres delivers at commit and drops on rollback.
///
/// # Arguments
/// * `conn` - the writer's transaction (`&mut tx`).
/// * `invalidations` - what the transaction changed; empty does nothing.
///
/// # Errors
/// [`OkfError::Database`]; the caller's transaction then fails as it would
/// for any other statement.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn invalidate(
    conn: &mut PgConnection,
    invalidations: &[Invalidation],
) -> Result<(), OkfError> {
    if invalidations.is_empty() {
        return Ok(());
    }
    catalogs::bump_admin_generation(&mut *conn).await?;
    let now = Utc::now().timestamp_millis();
    let metrics = Metrics::global();
    for invalidation in invalidations {
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(CHANNEL)
            .bind(invalidation.to_payload(now))
            .execute(&mut *conn)
            .await?;
        metrics.invalidation_sent(invalidation.kind());
    }
    Ok(())
}

/// Connects a listener on its own one-connection pool, subscribed to
/// [`CHANNEL`]. sqlx's `connect_with` keeps one connection from the pool it
/// is given for good, so the main pool would lose a request slot; the small
/// pool also gives the listener its reconnects.
///
/// # Arguments
/// * `pool` - the main pool, whose connect options the listener copies.
///
/// # Errors
/// [`OkfError::Database`] if the database cannot be reached.
#[tracing::instrument(level = "debug", skip_all)]
async fn connect(pool: &PgPool) -> Result<PgListener, OkfError> {
    let own = PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await?;
    let mut listener = PgListener::connect_with(&own).await?;
    listener.listen(CHANNEL).await?;
    Ok(listener)
}

/// Listens on [`CHANNEL`] until `shutdown`, applying each message to
/// `app`. A lost connection (`try_recv` returns `Ok(None)`, already
/// reconnected) marks every cache stale, since messages sent meanwhile are
/// lost (spec Decision 12). A connect failure backs off from 1 s to 30 s.
///
/// # Arguments
/// * `app` - the replica's shared state.
/// * `shutdown` - cancelled to stop.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "long-lived loop; each message gets its own root span"
    )
)]
pub async fn listen(app: Arc<App>, shutdown: CancellationToken) {
    let mut backoff = Backoff::new(BACKOFF_FIRST, BACKOFF_MAX);
    loop {
        let connected = tokio::select! {
            () = shutdown.cancelled() => return,
            connected = connect(app.pool()) => connected,
        };
        let mut listener = match connected {
            Ok(listener) => listener,
            Err(error) => {
                split_event!(
                    warn,
                    { kind = error.kind(), },
                    { %error, },
                    "the change feed could not connect; trying again"
                );
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = tokio::time::sleep(backoff.next_delay()) => continue,
                }
            }
        };
        // A fresh connection: anything sent while we had none is lost.
        app.apply_reconnect().await;
        backoff.reset();
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                received = listener.try_recv() => match received {
                    Ok(Some(notification)) => apply_payload(&app, notification.payload()).await,
                    // Lost and already re-established by sqlx.
                    Ok(None) => app.apply_reconnect().await,
                    // A failure sqlx could not repair: connect again.
                    Err(error) => {
                        let error = OkfError::from(error);
                        split_event!(
                            warn,
                            { kind = error.kind(), },
                            { %error, },
                            "the change feed connection failed; connecting again"
                        );
                        break;
                    }
                },
            }
        }
    }
}

/// Parses one payload and applies it; one that cannot be read is treated as
/// a reconnect, for safety (spec "Error handling").
///
/// # Arguments
/// * `app` - the replica's shared state.
/// * `payload` - the `NOTIFY` payload.
#[tracing::instrument(level = "debug", skip_all)]
async fn apply_payload(app: &App, payload: &str) {
    let metrics = Metrics::global();
    let Some(message) = Message::parse(payload) else {
        metrics.invalidation_unreadable();
        app.apply_reconnect().await;
        return;
    };
    let kind = message.invalidation.kind();
    metrics.invalidation_received(kind, message.lag(Utc::now().timestamp_millis()));
    // `parent: None`: a background root, like each poll pass.
    let span =
        tracing::info_span!(parent: None, "okf.invalidate.apply", okf.invalidation.kind = kind);
    app.apply(&message).instrument(span).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::pg_support::TestDb;
    use tokio::time::timeout;

    /// Longest a test waits for a notification.
    const WAIT: Duration = Duration::from_secs(30);

    #[test]
    fn every_kind_round_trips_through_its_payload() {
        let all = [
            Invalidation::Catalog {
                id: 42,
                generation: 17,
            },
            Invalidation::CatalogMeta {
                id: 42,
                name: "notes".into(),
            },
            Invalidation::Credential {
                key_id: "k_1".to_owned(),
            },
            Invalidation::User {
                id: "u_1".to_owned(),
            },
            Invalidation::Team {
                id: "t_1".to_owned(),
            },
            Invalidation::Settings,
        ];
        for invalidation in all {
            let payload = invalidation.to_payload(1_700_000_000_000);
            let message = Message::parse(&payload).expect("parses");
            assert_eq!(message.invalidation, invalidation);
            assert_eq!(message.sent_at_ms, 1_700_000_000_000);
        }
    }

    #[test]
    fn a_payload_of_an_unknown_version_or_kind_does_not_parse() {
        assert!(
            Message::parse(r#"{"v":2,"at":1,"kind":"catalog","id":1,"generation":1}"#).is_none()
        );
        assert!(Message::parse(r#"{"v":1,"at":1,"kind":"nope"}"#).is_none());
        assert!(Message::parse("42").is_none());
    }

    #[test]
    fn a_negative_lag_from_clock_skew_is_zero() {
        let message = Message::parse(&Invalidation::Settings.to_payload(2_000)).expect("parses");
        assert_eq!(message.lag(1_000), Duration::ZERO);
        assert_eq!(message.lag(2_500), Duration::from_millis(500));
    }

    #[test]
    fn a_payload_names_ids_only() {
        let payload = Invalidation::Credential {
            key_id: "k_1".to_owned(),
        }
        .to_payload(1);
        assert_eq!(
            payload,
            r#"{"v":1,"at":1,"kind":"credential","key_id":"k_1"}"#
        );
    }

    #[tokio::test]
    async fn a_committed_invalidation_is_delivered_and_a_rolled_back_one_is_not() {
        let db = TestDb::start_migrated().await;
        let mut listener = PgListener::connect_with(&db.pool).await.unwrap();
        listener.listen(CHANNEL).await.unwrap();

        let mut tx = db.pool.begin().await.unwrap();
        invalidate(&mut tx, &[Invalidation::Settings])
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        invalidate(
            &mut tx,
            &[Invalidation::CatalogMeta {
                id: 3,
                name: "c".into(),
            }],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        // The rolled-back message was never sent, so the first one received
        // is the committed one.
        let got = timeout(WAIT, listener.recv())
            .await
            .expect("a notification within the wait")
            .unwrap();
        let message = Message::parse(got.payload()).expect("parses");
        assert_eq!(
            message.invalidation,
            Invalidation::CatalogMeta {
                id: 3,
                name: "c".into()
            }
        );
    }

    #[tokio::test]
    async fn invalidate_bumps_admin_generation_once_per_call() {
        let db = TestDb::start_migrated().await;
        let before = catalogs::admin_generation(&db.pool).await.unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        invalidate(
            &mut tx,
            &[
                Invalidation::Settings,
                Invalidation::User { id: "u".into() },
            ],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            before + 1
        );

        // Nothing to invalidate: no bump.
        let mut tx = db.pool.begin().await.unwrap();
        invalidate(&mut tx, &[]).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            before + 1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_listener_applies_a_committed_message_to_the_caches() {
        use crate::clock::SystemClock;
        use crate::config::Config;
        use crate::store::keys as key_rows;

        let db = crate::db::test_db().await;
        let key = key_rows::insert_legacy_test_key(
            &db.pool,
            "reader",
            key_rows::Access::Read,
            "tester",
            Utc::now(),
        )
        .await;
        let app = App::load(db.pool.clone(), Config::default(), Arc::new(SystemClock))
            .await
            .unwrap();
        assert!(app.keys().find(&key).is_some());
        let shutdown = CancellationToken::new();
        let listener = tokio::spawn(listen(Arc::clone(&app), shutdown.clone()));

        // Revoke inside one transaction that also sends the message, as a
        // store function will after Task 2.
        let mut tx = db.pool.begin().await.unwrap();
        sqlx::query("UPDATE api_keys SET revoked_at = now() WHERE id = 'reader'")
            .execute(&mut *tx)
            .await
            .unwrap();
        invalidate(
            &mut tx,
            &[Invalidation::Credential {
                key_id: "reader".to_owned(),
            }],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        // The listener may still be connecting when the message is sent; its
        // first action on connecting is a full reload, so either path ends here.
        timeout(WAIT, async {
            while app.keys().find(&key).is_some() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the revoked key left the cache within the wait");

        shutdown.cancel();
        timeout(WAIT, listener)
            .await
            .expect("the listener stopped within the wait")
            .unwrap();
    }
}

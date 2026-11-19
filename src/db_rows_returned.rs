//! Test support: how many rows the database sent back to the code under
//! test, summed from sqlx's `sqlx::query` events (`rows_returned`). The
//! size tests of issues #728 and #736 use it to show a request reads a
//! bounded number of rows however many catalogs a user's teams own.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tracing_subscriber::layer::SubscriberExt;
// `with_filter` is a method of the `Layer` trait.
use tracing_subscriber::Layer as _;

/// Sums the `rows_returned` of every `sqlx::query` event, and counts the
/// statements.
#[derive(Clone, Default)]
pub(crate) struct RowsReturned {
    /// The rows returned, summed.
    pub rows: Arc<AtomicU64>,
    /// The statements seen.
    pub statements: Arc<AtomicU64>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for RowsReturned {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Rows(u64);
        impl tracing::field::Visit for Rows {
            fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
                if field.name() == "rows_returned" {
                    self.0 = value;
                }
            }
            fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
        }
        if event.metadata().target() != "sqlx::query" {
            return;
        }
        let mut rows = Rows(0);
        event.record(&mut rows);
        self.rows.fetch_add(rows.0, Ordering::Relaxed);
        self.statements.fetch_add(1, Ordering::Relaxed);
    }
}

/// Runs `work` on this thread with a subscriber that counts its rows.
///
/// # Returns
/// `work`'s output, the rows returned and the statements run while it ran
/// (only this thread's events: the work must not hand its queries to
/// another task).
pub(crate) async fn counting<F: Future>(work: F) -> (F::Output, u64, u64) {
    let counter = RowsReturned::default();
    let subscriber = tracing_subscriber::registry().with(
        counter.clone().with_filter(
            tracing_subscriber::filter::Targets::new()
                .with_target("sqlx::query", tracing::Level::TRACE),
        ),
    );
    let output = {
        let _guard = tracing::subscriber::set_default(subscriber);
        work.await
    };
    (
        output,
        counter.rows.load(Ordering::Relaxed),
        counter.statements.load(Ordering::Relaxed),
    )
}

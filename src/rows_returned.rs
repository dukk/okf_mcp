//! A tracing layer that counts the rows the database returns, for the
//! tests that prove a request reads a bounded number of rows (M18).

use std::sync::Arc;

/// Sums the `rows_returned` of every `sqlx::query` event: how many rows the
/// database sent back to the code under test.
#[derive(Clone, Default)]
pub(crate) struct RowsReturned {
    /// Rows the database returned.
    pub(crate) rows: Arc<std::sync::atomic::AtomicU64>,
    /// Statements that returned them.
    pub(crate) statements: Arc<std::sync::atomic::AtomicU64>,
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
        let relaxed = std::sync::atomic::Ordering::Relaxed;
        self.rows.fetch_add(rows.0, relaxed);
        self.statements.fetch_add(1, relaxed);
    }
}

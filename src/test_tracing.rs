//! Scoped `tracing` subscribers for tests that count or record what the code
//! under test emits (issue #481).
//!
//! `tracing-core` caches, per callsite, whether any subscriber wants it. While
//! only one subscriber is alive, a thread that reaches a callsite for the first
//! time computes that answer from its *own* default subscriber, not from every
//! live one. Test threads run side by side, so a thread with no subscriber can
//! register a callsite this test is about to use as "never" while the test's
//! scoped subscriber is already alive, and the test then records nothing.
//!
//! With a second live subscriber the answer is always computed from all of them.
//! These helpers keep one inert subscriber alive for the whole test process and
//! make it exist before any scoped subscriber does.

use std::sync::LazyLock;

use tracing::dispatcher::DefaultGuard;
use tracing::subscriber::NoSubscriber;
use tracing::{Dispatch, Subscriber};

/// An inert subscriber, alive until the process exits. Its only job is to
/// keep the registry of live subscribers above one entry.
static ANCHOR: LazyLock<Dispatch> = LazyLock::new(|| Dispatch::new(NoSubscriber::default()));

/// Registers `subscriber` as a live subscriber without making it any thread's
/// default, for a test that decides when and where to install it.
///
/// # Arguments
/// * `subscriber` - the subscriber the test inspects afterwards.
///
/// # Returns
/// The subscriber's dispatcher; it stays registered while this value lives.
pub(crate) fn keep_alive(subscriber: impl Subscriber + Send + Sync + 'static) -> Dispatch {
    LazyLock::force(&ANCHOR);
    Dispatch::new(subscriber)
}

/// Installs `subscriber` as this thread's default until the guard drops.
///
/// # Arguments
/// * `subscriber` - the subscriber the test inspects afterwards.
///
/// # Returns
/// The guard that restores the thread's previous default when dropped.
pub(crate) fn set_default(subscriber: impl Subscriber + Send + Sync + 'static) -> DefaultGuard {
    LazyLock::force(&ANCHOR);
    tracing::subscriber::set_default(subscriber)
}

/// Runs `body` with `subscriber` as this thread's default.
///
/// # Arguments
/// * `subscriber` - the subscriber the test inspects afterwards.
/// * `body` - the code under test.
///
/// # Returns
/// What `body` returns.
pub(crate) fn with_default<T>(
    subscriber: impl Subscriber + Send + Sync + 'static,
    body: impl FnOnce() -> T,
) -> T {
    LazyLock::force(&ANCHOR);
    tracing::subscriber::with_default(subscriber, body)
}

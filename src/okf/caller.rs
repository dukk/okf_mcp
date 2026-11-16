//! Carrying the calling thread's tracing context onto rayon workers.
//!
//! `tracing` keeps the current span and the scoped subscriber in thread-local
//! storage, and rayon's worker threads do not inherit them. Without help, a span
//! opened inside a parallel closure becomes a root span, detached from the
//! operation (and from the request or publish trace) that started it, and a
//! subscriber installed with `with_default` is absent on the workers.
//!
//! [`Caller::capture`] records the calling thread's span and dispatcher before a
//! parallel iterator; `map_init(|| caller.enter(), ...)` re-enters both once per
//! rayon work split on the worker thread (not once per element).

use tracing::dispatcher::DefaultGuard;
use tracing::span::EnteredSpan;
use tracing::{Dispatch, Span};

/// The span and dispatcher that were current where a parallel pass started.
///
/// `Span` and `Dispatch` are both `Send + Sync`, so a shared `&Caller` can be
/// captured by rayon closures (in Python terms: the closure holds a read-only
/// reference that every worker thread may use at once).
#[derive(Debug, Clone)]
pub(crate) struct Caller {
    span: Span,
    dispatch: Dispatch,
}

/// A worker thread's hold on the captured context; leaving scope restores the
/// thread's own span and dispatcher.
///
/// Field order matters: Rust drops fields in declaration order, so the span is
/// exited before the dispatcher guard is released.
#[must_use = "the context is left again as soon as this value is dropped"]
pub(crate) struct Entered {
    _span: EnteredSpan,
    _dispatch: DefaultGuard,
}

impl Caller {
    /// Captures the current span and dispatcher of the calling thread.
    ///
    /// Call it on the thread that owns the operation's span, before the
    /// parallel iterator starts.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "span maker: captures the enclosing span, so its own span would replace it"
        )
    )]
    pub(crate) fn capture() -> Self {
        Self {
            span: Span::current(),
            dispatch: tracing::dispatcher::get_default(Dispatch::clone),
        }
    }

    /// Enters the captured span and dispatcher on the current (worker) thread.
    ///
    /// Intended as the `init` closure of rayon's `map_init`, which runs it once
    /// per work split.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "span maker: enters the captured span, so its own span would replace it"
        )
    )]
    pub(crate) fn enter(&self) -> Entered {
        Entered {
            _dispatch: tracing::dispatcher::set_default(&self.dispatch),
            _span: self.span.clone().entered(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::thread::{self, ThreadId};

    use rayon::prelude::*;
    use tracing::span::Attributes;
    use tracing::{Id, Subscriber};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::registry::LookupSpan;
    use tracing_subscriber::Layer;

    use super::Caller;
    use crate::okf::snapshot::Snapshot;
    use crate::okf::workspace::MemWorkspace;

    /// One recorded span: its name, its parent's name and the creating thread.
    type Seen = (String, Option<String>, ThreadId);

    /// A layer that records every new span with its parent.
    struct Recorder(Arc<Mutex<Vec<Seen>>>);

    impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Recorder {
        fn on_new_span(&self, _attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
            let span = ctx.span(id).expect("a new span is registered");
            let parent = span.parent().map(|p| p.name().to_string());
            self.0.lock().expect("recorder lock").push((
                span.name().to_string(),
                parent,
                thread::current().id(),
            ));
        }
    }

    #[tracing::instrument(level = "trace", skip_all)]
    fn leaf() {}

    fn recording() -> (impl Subscriber + Send + Sync, Arc<Mutex<Vec<Seen>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            tracing_subscriber::registry().with(Recorder(seen.clone())),
            seen,
        )
    }

    fn pool() -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .expect("test pool")
    }

    fn named<'a>(seen: &'a [Seen], name: &str) -> Vec<&'a Seen> {
        seen.iter().filter(|(n, _, _)| n == name).collect()
    }

    #[test]
    fn workers_without_the_caller_start_root_spans() {
        let (subscriber, seen) = recording();
        // Arrange: a scoped subscriber, a parent span, and no capture.
        tracing::subscriber::with_default(subscriber, || {
            let _root = tracing::info_span!("root").entered();
            // Act: workers run the leaf with nothing carried over.
            pool().install(|| (0..16).into_par_iter().for_each(|_| leaf()));
        });
        // Assert: on the workers neither the scoped subscriber nor a parent is
        // visible, so nothing is recorded at all.
        assert!(named(&seen.lock().expect("lock"), "leaf").is_empty());
    }

    #[test]
    fn workers_keep_the_callers_span_and_subscriber() {
        let (subscriber, seen) = recording();
        let main_thread = thread::current().id();
        tracing::subscriber::with_default(subscriber, || {
            let root = tracing::info_span!("root");
            // Act: capture under `root`, then run the leaves on a 4-thread pool.
            let caller = root.in_scope(Caller::capture);
            pool().install(|| {
                (0..64)
                    .into_par_iter()
                    .with_max_len(1)
                    .map_init(|| caller.enter(), |_entered, _| leaf())
                    .collect::<Vec<_>>()
            });
        });
        // Assert: every worker span is a child of `root`, and none ran on the
        // test thread, so the context really crossed threads.
        let seen = seen.lock().expect("lock");
        let leaves = named(&seen, "leaf");
        assert_eq!(leaves.len(), 64);
        assert!(leaves
            .iter()
            .all(|(_, parent, _)| parent.as_deref() == Some("root")));
        assert!(leaves.iter().all(|(_, _, thread)| *thread != main_thread));
    }

    #[test]
    fn snapshot_build_documents_are_children_of_the_build_span() {
        let (subscriber, seen) = recording();
        let workspace = MemWorkspace::from_files((0..3000).map(|n| {
            // `from_files` borrows its strings, so leak them (test process only).
            let path: &'static str = Box::leak(format!("c{n}.md").into_boxed_str());
            (path, "---\ntype: note\n---\n\n# Title\n")
        }));
        // The subscriber is scoped to the thread that calls `build` (a pool
        // thread here); the other pool threads only get it through `Caller`.
        pool().install(|| {
            tracing::subscriber::with_default(subscriber, || {
                Snapshot::build(&workspace).expect("snapshot builds")
            })
        });
        let seen = seen.lock().expect("lock");
        let docs = named(&seen, "read_document");
        assert_eq!(docs.len(), 3000);
        assert!(docs
            .iter()
            .all(|(_, parent, _)| parent.as_deref() == Some("build")));
    }

    /// `mcp::blocking` runs its closure on a tokio blocking thread, which has
    /// neither the caller's span nor its scoped subscriber.
    #[tokio::test]
    async fn blocking_work_keeps_the_callers_span_and_subscriber() {
        let (subscriber, seen) = recording();
        let test_thread = thread::current().id();
        // A guard (not `with_default`) because the scope spans an `.await`.
        let _guard = tracing::subscriber::set_default(subscriber);
        let root = tracing::info_span!("root");
        // Act: run a span-opening closure through `blocking` under `root`.
        let _entered = root.enter();
        let worker = crate::mcp::blocking(|| {
            leaf();
            thread::current().id()
        })
        .await;
        // Assert: the leaf is a child of `root`, and it ran on another thread.
        assert_ne!(worker, test_thread);
        let seen = seen.lock().expect("lock");
        let leaves = named(&seen, "leaf");
        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].1.as_deref(), Some("root"));
    }
}

//! W3C trace context at the edges. Task 3 reads an inbound `traceparent` as
//! a span link ([`link_from`]); Task 6 adds the outbound injection.

use axum::http::HeaderMap;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::{SpanContext, TraceContextExt, TraceState};
use opentelemetry_http::HeaderExtractor;
use opentelemetry_sdk::propagation::TraceContextPropagator;

/// The caller's trace context from a request's `traceparent`, for a span
/// link. The request span stays a root: a client (or anyone, since the
/// header is unauthenticated) never chooses our trace id (spec Decision 4).
///
/// Only the ids and flags are kept. The header's `tracestate` is dropped, so
/// no header text travels into an exported link.
///
/// # Arguments
/// * `headers` - the request's headers.
///
/// # Returns
/// The remote span context, or `None` when the header is absent, malformed or
/// not valid W3C trace context.
#[tracing::instrument(level = "trace", skip_all)]
pub fn link_from(headers: &HeaderMap) -> Option<SpanContext> {
    // A propagator is a stateless parser; `extract` yields a context whose
    // span context is invalid when the header does not parse.
    let context = TraceContextPropagator::new().extract(&HeaderExtractor(headers));
    let remote = context.span().span_context().clone();
    remote.is_valid().then(|| {
        SpanContext::new(
            remote.trace_id(),
            remote.span_id(),
            remote.trace_flags(),
            true,
            TraceState::default(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_traceparent_becomes_a_remote_context_without_tracestate() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "traceparent",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
                .parse()
                .expect("a header value"),
        );
        headers.insert("tracestate", "vendor=zq-sentinel".parse().expect("value"));

        let link = link_from(&headers).expect("a link");

        assert_eq!(
            link.trace_id().to_string(),
            "0af7651916cd43dd8448eb211c80319c"
        );
        assert_eq!(link.span_id().to_string(), "b7ad6b7169203331");
        assert!(link.is_remote());
        assert!(link.is_sampled());
        assert_eq!(link.trace_state().header(), "");
    }

    #[test]
    fn a_missing_or_malformed_header_gives_no_link() {
        let mut headers = HeaderMap::new();
        assert!(link_from(&headers).is_none());
        headers.insert("traceparent", "garbage".parse().expect("value"));
        assert!(link_from(&headers).is_none());
        headers.insert(
            "traceparent",
            "00-00000000000000000000000000000000-b7ad6b7169203331-01"
                .parse()
                .expect("value"),
        );
        assert!(link_from(&headers).is_none());
    }
}

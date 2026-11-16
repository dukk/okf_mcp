//! Task 3: the HTTP request span, its principal ids and its metrics. Each
//! span test runs at two span filters (`info` and `trace`) through
//! [`FILTERS`], so Task 10 can re-run them over the swept tree.

use std::future::Future;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use opentelemetry::trace::{SpanId, Status};
use tower::ServiceExt;
use tracing::Instrument;

use super::*;
use crate::config::Config;
use crate::http::api::testing::{app, every_catalog_admin, send, Browser, Payload};
use crate::mcp::testing::{app_with_metrics, render, TestApp};
use crate::telemetry::config::LogFormat;
use crate::telemetry::subscriber::capture::{attribute, Observed, Observer};

/// The two span filters every test here runs at.
const FILTERS: [&str; 2] = ["info", "trace"];

/// A value that must never appear in an exported span.
const SENTINEL: &str = "zq-sentinel-5d1c";

/// Runs `body` under a subscriber with the span filter `filter` and hands
/// back what was exported.
async fn observed<Fut: Future<Output = ()>>(filter: &str, body: Fut) -> Observed {
    let observer = Observer::start("off", filter, "off", LogFormat::Text);
    body.await;
    observer.finish()
}

/// Sends `GET uri` with extra headers through the whole router.
async fn fetch(
    test: &TestApp,
    uri: &str,
    key: Option<&str>,
    headers: &[(&str, &str)],
) -> StatusCode {
    let mut request = Request::get(uri);
    if let Some(key) = key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    router(Arc::clone(&test.app), &CancellationToken::new())
        .oneshot(request.body(Body::empty()).expect("request"))
        .await
        .expect("the router answers")
        .status()
}

/// The text of every exported span, for "never contains" checks.
fn dump(observed: &Observed) -> String {
    format!("{:?}", observed.spans)
}

#[tokio::test]
async fn a_request_span_names_method_and_route() {
    let test = app().await;
    let key = every_catalog_admin(&test).await;
    for filter in FILTERS {
        let observed = observed(filter, async {
            let uri = format!("/api/v1/catalogs/main/health?{SENTINEL}=1");
            let status = fetch(&test, &uri, Some(&key), &[("x-probe", SENTINEL)]).await;
            assert_eq!(status, StatusCode::OK, "{filter}");
            fetch(
                &test,
                &format!("/api/v1/catalogs/{SENTINEL}/health"),
                Some(&key),
                &[],
            )
            .await;
        })
        .await;

        let spans = observed.spans_named("GET /api/v1/catalogs/{catalog}/health");
        assert_eq!(spans.len(), 2, "{filter}: {}", dump(&observed));
        let ok = spans
            .iter()
            .find(|span| attribute(span, "http.response.status_code") == Some(&200.into()))
            .unwrap_or_else(|| panic!("{filter}: a 200 span"));
        assert_eq!(
            attribute(ok, "http.request.method")
                .map(ToString::to_string)
                .as_deref(),
            Some("GET")
        );
        assert_eq!(
            attribute(ok, "http.route")
                .map(ToString::to_string)
                .as_deref(),
            Some("/api/v1/catalogs/{catalog}/health")
        );
        assert_eq!(ok.status, Status::Unset, "{filter}");
        assert!(
            !dump(&observed).contains(SENTINEL),
            "{filter}: {}",
            dump(&observed)
        );
        assert!(
            !dump(&observed).contains(&key),
            "{filter}: the bearer value leaked"
        );
    }
}

#[tokio::test]
async fn a_5xx_is_an_error_and_a_4xx_is_not() {
    let test = app().await;
    let key = every_catalog_admin(&test).await;
    for filter in FILTERS {
        let observed = observed(filter, async {
            let missing = fetch(&test, "/api/v1/catalogs/nope/health", Some(&key), &[]).await;
            assert_eq!(missing, StatusCode::NOT_FOUND);
        })
        .await;
        let span = &observed.spans_named("GET /api/v1/catalogs/{catalog}/health")[0];
        assert_eq!(
            attribute(span, "http.response.status_code"),
            Some(&404.into()),
            "{filter}"
        );
        assert_eq!(
            span.status,
            Status::Unset,
            "{filter}: a 4xx is not an error"
        );
    }
    test.app.pool().close().await;
    for filter in FILTERS {
        let observed = observed(filter, async {
            let status = fetch(&test, "/healthz", None, &[]).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        })
        .await;
        let span = &observed.spans_named("GET /healthz")[0];
        assert_eq!(
            attribute(span, "http.response.status_code"),
            Some(&503.into()),
            "{filter}: the status is an i64"
        );
        assert!(matches!(span.status, Status::Error { .. }), "{filter}");
    }
}

#[tokio::test]
async fn an_unmatched_path_makes_no_span() {
    let test = app().await;
    for filter in FILTERS {
        let observed = observed(filter, async {
            let status = fetch(&test, &format!("/no/such/{SENTINEL}"), None, &[]).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
        })
        .await;
        assert!(observed.spans.is_empty(), "{filter}: {}", dump(&observed));
    }
}

#[tokio::test]
async fn a_forged_traceparent_is_a_link_not_the_parent() {
    let test = app().await;
    let forged = "0af7651916cd43dd8448eb211c80319c";
    let header = format!("00-{forged}-b7ad6b7169203331-01");
    for filter in FILTERS {
        let observed = observed(filter, async {
            fetch(&test, "/healthz", None, &[("traceparent", &header)]).await;
        })
        .await;
        let span = &observed.spans_named("GET /healthz")[0];
        assert_ne!(span.span_context.trace_id().to_string(), forged, "{filter}");
        assert_eq!(span.parent_span_id, SpanId::INVALID, "{filter}: a root");
        assert_eq!(span.links.links.len(), 1, "{filter}");
        assert_eq!(
            span.links.links[0].span_context.trace_id().to_string(),
            forged,
            "{filter}"
        );
    }
}

#[tokio::test]
async fn a_request_span_is_a_root_even_inside_another_span() {
    let test = app().await;
    for filter in FILTERS {
        let observed = observed(filter, async {
            async { fetch(&test, "/healthz", None, &[]).await }
                .instrument(tracing::info_span!("ambient"))
                .await;
        })
        .await;
        let span = &observed.spans_named("GET /healthz")[0];
        assert_eq!(span.parent_span_id, SpanId::INVALID, "{filter}");
        let ambient = &observed.spans_named("ambient")[0];
        assert_ne!(
            span.span_context.trace_id(),
            ambient.span_context.trace_id(),
            "{filter}"
        );
    }
}

#[tokio::test]
async fn an_authenticated_request_records_principal_ids() {
    let test = app().await;
    let key = every_catalog_admin(&test).await;
    let user_id: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(crate::db::TEST_USER)
        .fetch_one(test.app.pool())
        .await
        .expect("the key's user");
    let key_id: String = sqlx::query_scalar("SELECT id FROM api_keys")
        .fetch_one(test.app.pool())
        .await
        .expect("the one key");
    for filter in FILTERS {
        let observed = observed(filter, async {
            let status = fetch(&test, "/api/v1/catalogs/main/health", Some(&key), &[]).await;
            assert_eq!(status, StatusCode::OK);
        })
        .await;
        let span = &observed.spans_named("GET /api/v1/catalogs/{catalog}/health")[0];
        assert_eq!(
            attribute(span, "enduser.id").map(ToString::to_string),
            Some(user_id.clone()),
            "{filter}"
        );
        assert_eq!(
            attribute(span, "okf.key.id")
                .map(ToString::to_string)
                .as_deref(),
            Some(key_id.as_str()),
            "{filter}"
        );
        assert!(
            !dump(&observed).contains(&key),
            "{filter}: the bearer value leaked"
        );
    }
}

/// `require_key` run under an extra `trace` span, standing in for the
/// sweep's `#[instrument]` on the auth middleware.
async fn spanned_require_key(
    State(app): State<Arc<App>>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    crate::auth::require_key(State(app), request, next)
        .instrument(tracing::trace_span!("helper"))
        .await
}

#[tokio::test]
async fn principal_ids_land_on_the_request_span_under_helper_spans() {
    let test = app().await;
    let key = every_catalog_admin(&test).await;
    let key_id: String = sqlx::query_scalar("SELECT id FROM api_keys")
        .fetch_one(test.app.pool())
        .await
        .expect("the one key");
    // The layers in the production order: the key layer on the route, then
    // the telemetry middleware, then `TraceLayer` outermost.
    let probe = || {
        axum::Router::new()
            .route("/probe", get(|| async { "ok" }))
            .route_layer(middleware::from_fn_with_state(
                Arc::clone(&test.app),
                spanned_require_key,
            ))
            .layer(middleware::from_fn_with_state(
                Arc::clone(&test.app),
                request_telemetry,
            ))
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(request_span)
                    .on_response(record_response),
            )
            .with_state(Arc::clone(&test.app))
    };
    for filter in FILTERS {
        let observed = observed(filter, async {
            let request = Request::get("/probe")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .expect("request");
            let response = probe().oneshot(request).await.expect("answers");
            assert_eq!(response.status(), StatusCode::OK);
        })
        .await;
        let span = &observed.spans_named("GET /probe")[0];
        assert!(
            attribute(span, "enduser.id").is_some(),
            "{filter}: {}",
            dump(&observed)
        );
        assert_eq!(
            attribute(span, "okf.key.id")
                .map(ToString::to_string)
                .as_deref(),
            Some(key_id.as_str()),
            "{filter}"
        );
        // At `trace` the helper span is exported, as a child.
        assert_eq!(
            observed.spans_named("helper").len(),
            usize::from(filter == "trace"),
            "{filter}"
        );
    }
}

#[tokio::test]
async fn a_console_session_records_the_user_id_and_never_the_user_name() {
    let test = app().await;
    let root = every_catalog_admin(&test).await;
    let name = "zq-sentinel-alice";
    let (created, _) = send(
        &test,
        "POST",
        "/api/v1/users",
        Some(&root),
        Some(serde_json::json!({"name": name})),
    )
    .await;
    assert_eq!(created.as_u16(), 201);
    let user_id: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(name)
        .fetch_one(test.app.pool())
        .await
        .expect("the new user");
    let mut browser = Browser::session_for(&test, name).await;
    for filter in FILTERS {
        let observed = observed(filter, async {
            let reply = browser
                .send(&test, "GET", "/api/v1/session", Payload::Empty)
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{filter}");
        })
        .await;
        let span = &observed.spans_named("GET /api/v1/session")[0];
        assert_eq!(
            attribute(span, "enduser.id").map(ToString::to_string),
            Some(user_id.clone()),
            "{filter}"
        );
        assert_eq!(
            attribute(span, "okf.key.id")
                .map(ToString::to_string)
                .as_deref(),
            Some("session"),
            "{filter}"
        );
        assert!(
            !dump(&observed).contains(name),
            "{filter}: {}",
            dump(&observed)
        );
    }
}

#[tokio::test]
async fn http_requests_are_measured_by_method_route_and_status() {
    let (test, registry) = app_with_metrics(&[("main", &[])], Config::default()).await;
    let router = router(Arc::clone(&test.app), &CancellationToken::new());
    for uri in ["/healthz", "/healthz", &format!("/no/such/{SENTINEL}")] {
        let request = Request::get(uri).body(Body::empty()).expect("request");
        router.clone().oneshot(request).await.expect("answers");
    }

    let text = render(&registry);

    let count = text
        .lines()
        .find(|line| line.starts_with("http_server_request_duration_seconds_count{"))
        .unwrap_or_else(|| panic!("no duration series in {text}"));
    assert!(count.contains(r#"http_route="/healthz""#), "{count}");
    assert!(count.contains(r#"http_request_method="GET""#), "{count}");
    assert!(
        count.contains(r#"http_response_status_code="200""#),
        "{count}"
    );
    assert!(count.ends_with(" 2"), "{count}");
    assert!(
        !text.contains(SENTINEL),
        "an unmatched path is not measured: {text}"
    );
    // Every request finished, so none is in flight.
    assert!(
        text.lines()
            .filter(|line| line.starts_with("http_server_active_requests{"))
            .all(|line| line.ends_with(" 0")),
        "{text}"
    );
}

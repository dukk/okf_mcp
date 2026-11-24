//! Task 3: the `mcp <tool>` span, its error kind, and the tool and session
//! metrics, through a real MCP session over a loopback socket.

use std::sync::Arc;
use std::time::Duration;

use opentelemetry::trace::Status;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use super::*;
use crate::config::Config;
use crate::mcp::testing::{app_with_metrics, render, McpProbe, TestApp};
use crate::store::keys::{self, Access, Grants, NewKey};
use crate::telemetry::config::LogFormat;
use crate::telemetry::subscriber::capture::{attribute, Observer};

const FILTERS: [&str; 2] = ["info", "trace"];

/// A value that must never appear in an exported span or a metric.
const SENTINEL: &str = "zq-sentinel-8e2b";

/// A served app, its key, and the means to stop it.
struct Served {
    test: TestApp,
    registry: prometheus::Registry,
    endpoint: String,
    key: String,
    shutdown: CancellationToken,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Served {
    /// Starts `http::serve` for an app holding catalog `pg`, with a read key
    /// named `reader`. A `#[tokio::test]` runtime is current-thread, so the
    /// server's tasks run on the test's thread and see its subscriber.
    async fn start() -> Self {
        let (test, registry) = app_with_metrics(
            &[(
                "pg",
                &[(
                    "a.md",
                    "---\ntype: Note\ntitle: Alpha\n---\n# Alpha\nText.\n",
                )],
            )],
            Config::default(),
        )
        .await;
        let key = keys::create(
            test.app.accounts().executor(),
            &NewKey {
                name: "reader",
                access: Access::Read,
                maintenance_report: false,
                grants: Grants::Catalogs(&["pg".into()]),
                default_catalog: None,
                actor: "reader",
                user: crate::db::TEST_USER,
                self_issued: false,
                expires_at: None,
                levels: &[],
            },
            &crate::store::audit::AuditActor::cli(),
            chrono::Utc::now(),
        )
        .await
        .expect("create a key");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let endpoint = format!("http://{}/mcp", listener.local_addr().expect("address"));
        let shutdown = CancellationToken::new();
        let server = tokio::spawn(crate::http::serve_with(
            Arc::clone(&test.app),
            listener,
            shutdown.clone(),
            crate::http::ServeOptions::NO_WORKER,
        ));
        Self {
            test,
            registry,
            endpoint,
            key,
            shutdown,
            server,
        }
    }

    /// Stops the server and waits for it, bounded.
    async fn stop(self) {
        self.shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(30), self.server)
            .await
            .expect("the server stops")
            .expect("the server task")
            .expect("the server returns");
    }
}

#[tokio::test]
async fn every_tool_call_opens_one_mcp_span() {
    let served = Served::start().await;
    for filter in FILTERS {
        let observer = Observer::start("off", filter, "off", LogFormat::Text);
        let probe = McpProbe::open(&served.endpoint, &served.key).await;
        let answer = probe
            .call(
                "get_concept",
                serde_json::json!({"path": "/a.md", "catalog": "pg"}),
            )
            .await;
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        let unloaded = probe
            .call(
                "get_concept",
                serde_json::json!({"path": "/a.md", "catalog": SENTINEL}),
            )
            .await;
        assert!(unloaded.get("result").is_some(), "{unloaded}");
        let key_id: String = sqlx::query_scalar("SELECT id FROM api_keys")
            .fetch_one(served.test.app.accounts().executor())
            .await
            .expect("the one key");
        probe.close().await;
        let observed = observer.finish();

        let spans = observed.spans_named("mcp get_concept");
        assert_eq!(spans.len(), 2, "{filter}: {:?}", observed.spans);
        let loaded = spans
            .iter()
            .find(|span| {
                attribute(span, "okf.catalog")
                    .map(ToString::to_string)
                    .as_deref()
                    == Some("pg")
            })
            .unwrap_or_else(|| panic!("{filter}: a span for pg"));
        assert_eq!(
            attribute(loaded, "okf.tool")
                .map(ToString::to_string)
                .as_deref(),
            Some("get_concept")
        );
        assert_eq!(
            attribute(loaded, "okf.outcome")
                .map(ToString::to_string)
                .as_deref(),
            Some("ok")
        );
        assert_eq!(
            attribute(loaded, "okf.key.id")
                .map(ToString::to_string)
                .as_deref(),
            Some(key_id.as_str())
        );
        assert!(attribute(loaded, "enduser.id").is_some(), "{filter}");
        assert_ne!(loaded.status, Status::error(""), "{filter}");
        // An unloaded catalog name is never an attribute.
        let none = spans
            .iter()
            .find(|span| {
                attribute(span, "okf.catalog")
                    .map(ToString::to_string)
                    .as_deref()
                    == Some("none")
            })
            .unwrap_or_else(|| panic!("{filter}: a span for no catalog"));
        assert_eq!(none.name, "mcp get_concept");
        assert!(
            !format!("{:?}", observed.spans).contains(SENTINEL),
            "{filter}: the typed catalog name leaked"
        );
    }
    served.stop().await;
}

#[tokio::test]
async fn a_tool_error_records_its_kind_not_its_message() {
    let served = Served::start().await;
    for filter in FILTERS {
        let observer = Observer::start("off", filter, "off", LogFormat::Text);
        let probe = McpProbe::open(&served.endpoint, &served.key).await;
        let answer = probe
            .call(
                "get_concept",
                serde_json::json!({"path": format!("/{SENTINEL}.md"), "catalog": "pg"}),
            )
            .await;
        assert_eq!(answer["result"]["isError"], true, "{answer}");
        probe.close().await;
        let observed = observer.finish();

        let span = &observed.spans_named("mcp get_concept")[0];
        assert_eq!(
            attribute(span, "okf.outcome")
                .map(ToString::to_string)
                .as_deref(),
            Some("error"),
            "{filter}"
        );
        assert_eq!(
            attribute(span, "okf.error.kind")
                .map(ToString::to_string)
                .as_deref(),
            Some("NotFound"),
            "{filter}"
        );
        assert!(matches!(span.status, Status::Error { .. }), "{filter}");
        assert!(
            !format!("{:?}", observed.spans).contains(SENTINEL),
            "{filter}: the path leaked"
        );
    }
    served.stop().await;
}

/// `tool_error` records the kind on the tool's own span even when a helper
/// span (as the sweep adds) is current.
#[tokio::test]
async fn error_kind_lands_on_the_tool_span_under_helper_spans() {
    for filter in FILTERS {
        let observer = Observer::start("off", filter, "off", LogFormat::Text);
        let span = tracing::info_span!("mcp", okf.error.kind = tracing::field::Empty,);
        TOOL_SPAN
            .scope(span.clone(), async {
                async {
                    let _ = tool_error(&OkfError::NotFound {
                        path: "/x.md".to_string(),
                    });
                }
                .instrument(tracing::debug_span!("tool method"))
                .await;
            })
            .instrument(span)
            .await;
        let observed = observer.finish();

        let tool = &observed.spans_named("mcp")[0];
        assert_eq!(
            attribute(tool, "okf.error.kind")
                .map(ToString::to_string)
                .as_deref(),
            Some("NotFound"),
            "{filter}"
        );
    }
}

#[tokio::test]
async fn an_unknown_tool_name_is_labelled_other() {
    let served = Served::start().await;
    let probe = McpProbe::open(&served.endpoint, &served.key).await;

    let answer = probe.call(SENTINEL, serde_json::json!({})).await;
    probe.close().await;

    assert!(answer.get("error").is_some(), "{answer}");
    let text = render(&served.registry);
    let line = text
        .lines()
        .find(|line| line.starts_with("okf_mcp_tool_duration_seconds_count{"))
        .unwrap_or_else(|| panic!("no tool series in {text}"));
    assert!(line.contains(r#"tool="other""#), "{line}");
    assert!(line.contains(r#"outcome="error""#), "{line}");
    assert!(!text.contains(SENTINEL), "{text}");
    served.stop().await;
}

#[tokio::test]
async fn tool_duration_is_recorded_per_tool_catalog_outcome() {
    let served = Served::start().await;
    let probe = McpProbe::open(&served.endpoint, &served.key).await;
    probe
        .call(
            "get_concept",
            serde_json::json!({"path": "/a.md", "catalog": "pg"}),
        )
        .await;
    probe
        .call(
            "get_concept",
            serde_json::json!({"path": "/missing.md", "catalog": "pg"}),
        )
        .await;
    probe.close().await;

    let text = render(&served.registry);

    let series = |tool: &str, outcome: &str| {
        text.lines()
            .filter(|line| line.starts_with("okf_mcp_tool_duration_seconds_count{"))
            .find(|line| {
                line.contains(&format!(r#"tool="{tool}""#))
                    && line.contains(r#"catalog="pg""#)
                    && line.contains(&format!(r#"outcome="{outcome}""#))
            })
            .map(str::to_string)
    };
    assert!(
        series("get_concept", "ok").is_some_and(|line| line.ends_with(" 1")),
        "{text}"
    );
    assert!(
        series("get_concept", "error").is_some_and(|line| line.ends_with(" 1")),
        "{text}"
    );
    assert!(
        text.contains("okf_mcp_tool_duration_seconds_bucket{"),
        "{text}"
    );
    served.stop().await;
}

#[tokio::test]
async fn an_open_session_counts_until_it_closes() {
    let served = Served::start().await;
    let gauge = |text: &str| {
        text.lines()
            .find(|line| line.starts_with("okf_mcp_sessions_active"))
            .map(str::to_string)
    };
    let probe = McpProbe::open(&served.endpoint, &served.key).await;
    assert!(
        gauge(&render(&served.registry)).is_some_and(|line| line.ends_with(" 1")),
        "{}",
        render(&served.registry)
    );

    probe.close().await;

    // rmcp drops the handler after it has torn the session down.
    tokio::time::timeout(Duration::from_secs(30), async {
        while !gauge(&render(&served.registry)).is_some_and(|line| line.ends_with(" 0")) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the session gauge returns to 0");
    served.stop().await;
}

/// The `mcp <tool>` span is a child of the `POST /mcp` request span in the
/// same trace, and at `trace` the tool's own spans nest under it (issue #523).
#[tokio::test]
async fn the_tool_span_is_a_child_of_the_request_span() {
    let served = Served::start().await;
    for filter in FILTERS {
        let observer = Observer::start("off", filter, "off", LogFormat::Text);
        let probe = McpProbe::open(&served.endpoint, &served.key).await;
        let answer = probe
            .call(
                "get_concept",
                serde_json::json!({"path": "/a.md", "catalog": "pg"}),
            )
            .await;
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        probe.close().await;
        let observed = observer.finish();

        let tool = &observed.spans_named("mcp get_concept")[0];
        let parent = observed
            .spans
            .iter()
            .find(|span| span.span_context.span_id() == tool.parent_span_id)
            .unwrap_or_else(|| panic!("{filter}: the tool span has no exported parent"));
        assert_eq!(parent.name, "POST /mcp", "{filter}");
        assert_eq!(
            parent.span_context.trace_id(),
            tool.span_context.trace_id(),
            "{filter}: the tool span is in another trace"
        );
        if filter == "trace" {
            let children = observed
                .spans
                .iter()
                .filter(|span| span.parent_span_id == tool.span_context.span_id())
                .count();
            assert!(children > 0, "trace: no span under the tool span");
        }
    }
    served.stop().await;
}

/// Without a request span in the extensions the parent is the current span,
/// and with none of those the tool span is a root; with one it is the request
/// span even when another span is current.
#[tokio::test]
async fn the_tool_span_parent_falls_back_to_current_then_root() {
    let _observer = Observer::start("off", "info", "off", LogFormat::Text);
    let request = tracing::info_span!("request");
    let mut parts = axum::http::Request::new(()).into_parts().0;
    parts
        .extensions
        .insert(crate::http::RequestSpan(request.clone()));
    let mut with_request = rmcp::model::Extensions::new();
    with_request.insert(parts);
    let without = rmcp::model::Extensions::new();

    assert_eq!(tool_span_parent(&without), None);
    let current = tracing::info_span!("current");
    let _entered = current.enter();
    assert_eq!(tool_span_parent(&without), current.id());
    assert_eq!(tool_span_parent(&with_request), request.id());
}

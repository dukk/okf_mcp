//! The example Grafana dashboard (`examples/grafana/okf-mcp.json`) names only
//! metrics the server exports, so renaming an instrument breaks this test
//! rather than a dashboard nobody opens until an incident.
//!
//! The list of exported families is `Metrics::ALL_FAMILIES`, which a unit
//! test in `src/telemetry/metrics.rs` keeps equal to what the instruments
//! render.

use std::collections::BTreeSet;

use okf_mcp::telemetry::metrics::Metrics;
use regex::Regex;
use serde_json::Value;

/// The dashboard file, read at compile time so the test needs no working
/// directory.
const DASHBOARD: &str = include_str!("../examples/grafana/okf-mcp.json");

/// PromQL words that can stand where a metric name could: grouping and
/// matching keywords. (Function names are skipped by being followed by `(`.)
const KEYWORDS: [&str; 8] = [
    "by",
    "without",
    "on",
    "ignoring",
    "group_left",
    "group_right",
    "bool",
    "offset",
];

/// Every `expr` in the dashboard, panels and nested rows alike.
fn expressions(node: &Value, found: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            for (key, value) in map {
                match (key.as_str(), value) {
                    ("expr", Value::String(text)) => found.push(text.clone()),
                    _ => expressions(value, found),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| expressions(item, found)),
        _ => {}
    }
}

/// The metric names a PromQL expression selects: its identifiers once label
/// matchers, grouping clauses, strings and durations are removed, minus
/// function names and keywords.
fn metric_names(expression: &str) -> Vec<String> {
    let strings = Regex::new(r#""[^"]*""#).expect("pattern");
    let braces = Regex::new(r"\{[^}]*\}").expect("pattern");
    let grouping = Regex::new(r"\b(?:by|without)\s*\([^)]*\)").expect("pattern");
    let ranges = Regex::new(r"\[[^\]]*\]").expect("pattern");
    let identifier = Regex::new(r"[A-Za-z_:][A-Za-z0-9_:]*(\s*\()?").expect("pattern");

    let cleaned = strings.replace_all(expression, "");
    let cleaned = braces.replace_all(&cleaned, "");
    let cleaned = grouping.replace_all(&cleaned, "");
    let cleaned = ranges.replace_all(&cleaned, "");
    identifier
        .captures_iter(&cleaned)
        .filter(|capture| capture.get(1).is_none())
        .map(|capture| capture[0].to_string())
        .filter(|name| !KEYWORDS.contains(&name.as_str()))
        .collect()
}

/// The family a series belongs to: a histogram's `_bucket`, `_sum` and
/// `_count` series belong to the histogram's family.
fn family_of(series: &str) -> &str {
    if Metrics::ALL_FAMILIES.contains(&series) {
        return series;
    }
    ["_bucket", "_sum", "_count"]
        .iter()
        .find_map(|suffix| series.strip_suffix(suffix))
        .filter(|base| Metrics::ALL_FAMILIES.contains(base))
        .unwrap_or(series)
}

#[test]
fn every_metric_in_the_dashboard_is_one_the_server_exports() {
    let dashboard: Value = serde_json::from_str(DASHBOARD).expect("the dashboard is JSON");
    let mut expressions_found = Vec::new();
    expressions(&dashboard, &mut expressions_found);
    assert!(expressions_found.len() >= 10, "found {expressions_found:?}");

    let mut used = BTreeSet::new();
    for expression in &expressions_found {
        let names = metric_names(expression);
        assert!(!names.is_empty(), "no metric in {expression}");
        for name in names {
            let family = family_of(&name);
            assert!(
                Metrics::ALL_FAMILIES.contains(&family),
                "{name} in `{expression}` is not an exported metric"
            );
            used.insert(family.to_string());
        }
    }
    // The dashboard covers the areas the issue lists, not one family.
    for expected in [
        "http_server_request_duration_seconds",
        "okf_mcp_tool_duration_seconds",
        "okf_publish_duration_seconds",
        "okf_poll_duration_seconds",
        "okf_auth_attempts_total",
        "okf_mail_jobs_total",
        "okf_telemetry_export_failures_total",
        "okf_clock_offset_seconds",
    ] {
        assert!(
            used.contains(expected),
            "{expected} is not on the dashboard"
        );
    }
}

/// Every selector of a metric in `expression`, matchers included
/// (`name{...}`), or the bare name when it has none.
fn selectors(expression: &str) -> Vec<String> {
    let pattern =
        Regex::new(r"\b(?:okf_|http_server_|db_client_)[a-z_]+(?:\{[^}]*\})?").expect("pattern");
    pattern
        .find_iter(expression)
        .map(|found| found.as_str().to_string())
        .collect()
}

#[test]
fn every_selector_is_scoped_by_the_job_variable() {
    // Standard OTel names (`http_server_request_duration_seconds`) are shared
    // by other services, so an unscoped query would merge them in.
    let dashboard: Value = serde_json::from_str(DASHBOARD).expect("the dashboard is JSON");
    let mut found = Vec::new();
    expressions(&dashboard, &mut found);
    for expression in &found {
        let chosen = selectors(expression);
        assert!(!chosen.is_empty(), "no selector in {expression}");
        for selector in chosen {
            assert!(
                selector.contains(r#"job=~"$job""#),
                "{selector} in `{expression}` is not scoped by $job"
            );
        }
    }
    let variables = dashboard["templating"]["list"]
        .as_array()
        .expect("template variables");
    assert!(variables.iter().any(|variable| variable["name"] == "job"));
}

#[test]
fn the_extractor_reads_labelled_selectors_with_variables() {
    let expression = r#"sum by (le) (rate(okf_poll_duration_seconds_bucket{job=~"$job",outcome="error"}[$__rate_interval])) / okf_clock_offset_seconds{job=~"$job"}"#;
    assert_eq!(
        metric_names(expression),
        [
            "okf_poll_duration_seconds_bucket",
            "okf_clock_offset_seconds"
        ]
    );
    assert_eq!(selectors(expression).len(), 2);
    assert!(selectors("okf_mail_queue_depth")[0].contains("okf_mail_queue_depth"));
}

#[test]
fn the_extractor_reads_promql_the_way_the_dashboard_uses_it() {
    let names = metric_names(
        r#"histogram_quantile(0.95, sum by (le, tool) (rate(okf_mcp_tool_duration_seconds_bucket{outcome="ok"}[5m])))"#,
    );
    assert_eq!(names, ["okf_mcp_tool_duration_seconds_bucket"]);
    assert_eq!(
        family_of("okf_mcp_tool_duration_seconds_bucket"),
        "okf_mcp_tool_duration_seconds"
    );
    assert_eq!(
        family_of("okf_no_such_metric_total"),
        "okf_no_such_metric_total"
    );
    let two = metric_names("sum(a_total) / sum(b_total) > 0.5");
    assert_eq!(two, ["a_total", "b_total"]);
}

#[test]
fn a_renamed_metric_would_fail_the_check() {
    let renamed = DASHBOARD.replace("okf_auth_attempts_total", "okf_sign_ins_total");
    let dashboard: Value = serde_json::from_str(&renamed).expect("JSON");
    let mut found = Vec::new();
    expressions(&dashboard, &mut found);
    let unknown = found
        .iter()
        .flat_map(|expression| metric_names(expression))
        .filter(|name| !Metrics::ALL_FAMILIES.contains(&family_of(name)))
        .collect::<Vec<_>>();
    assert_eq!(unknown, ["okf_sign_ins_total"]);
}

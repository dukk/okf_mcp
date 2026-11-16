//! With `OTEL_SDK_DISABLED=true` in the real process environment, OTLP is
//! off but the Prometheus exposition is still populated.
//!
//! Its own integration-test binary: it sets an environment variable and the
//! global meter provider, which a second test in the process would share.

use okf_mcp::telemetry::config::{OtlpConfig, TelemetryConfig};
use okf_mcp::telemetry::metrics::Metrics;
use okf_mcp::telemetry::Telemetry;

#[tokio::test]
async fn a_disabled_sdk_still_serves_the_initial_families() {
    std::env::set_var("OTEL_SDK_DISABLED", "true");
    let config = TelemetryConfig {
        otlp: Some(OtlpConfig {
            sentry_dsn_env: Some("DSN_NOT_SET_FOR_TEST".to_string()),
            ..OtlpConfig::default()
        }),
        ..TelemetryConfig::default()
    };

    // Reads the real environment, as `serve` does; the unset DSN variable
    // proves a disabled SDK needs no credentials.
    let telemetry = Telemetry::start(&config, &|name| std::env::var(name).ok()).expect("starts");
    // `start` set the global meter provider, which `App::load` builds on.
    let _metrics = Metrics::global();

    assert!(!telemetry.traces_enabled() && !telemetry.logs_enabled());
    let families = telemetry.registry().gather();
    for expected in Metrics::FAMILIES {
        assert!(
            families.iter().any(|family| family.name() == *expected),
            "{expected} missing from the exposition"
        );
    }
    let failures = families
        .iter()
        .find(|family| family.name() == "okf_telemetry_export_failures_total")
        .expect("the failure family");
    assert_eq!(failures.get_metric().len(), 12);
    let report = telemetry.shutdown().await;
    assert!(report.all_flushed(), "{report:?}");
}

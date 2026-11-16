//! The `[telemetry]` table (spec "Configuration"). Secrets are never here:
//! only the names of the environment variables that hold them, read when
//! `serve` starts ([`super::otlp::OtlpTarget::resolve`]).

use std::net::SocketAddr;
use std::time::Duration;

use serde::Deserialize;
use tracing_subscriber::EnvFilter;

/// The service name OTLP reports when `service_name` is unset.
pub const DEFAULT_SERVICE_NAME: &str = "okf-mcp";
/// stderr's filter when neither `RUST_LOG` nor `[telemetry.stderr] filter` is set.
pub const DEFAULT_STDERR_FILTER: &str = "warn,okf_mcp=info,tower_http=info";
/// The OTLP span and log filters' default.
pub const DEFAULT_OTLP_FILTER: &str = "info";
/// How often metrics are pushed over OTLP by default.
pub const DEFAULT_EXPORT_INTERVAL: Duration = Duration::from_secs(60);

/// `[telemetry]`.
// `deny_unknown_fields` (plan Decision 7): a misspelt key would otherwise
// silently turn a setting off. (The rest of `Config` ignores unknown keys on
// purpose, so a newer file still loads on an older binary; this table is new,
// so there is no such file.)
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetryConfig {
    /// `service.name`.
    pub service_name: String,
    /// `deployment.environment.name`, when set.
    pub environment: Option<String>,
    /// `[telemetry.stderr]`.
    pub stderr: StderrConfig,
    /// `[telemetry.otlp]`; OTLP is off without it.
    pub otlp: Option<OtlpConfig>,
    /// `[telemetry.prometheus]`; no `/metrics` listener without it.
    pub prometheus: Option<PrometheusConfig>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            service_name: DEFAULT_SERVICE_NAME.to_string(),
            environment: None,
            stderr: StderrConfig::default(),
            otlp: None,
            prometheus: None,
        }
    }
}

/// `[telemetry.prometheus]`: the separate, unauthenticated `/metrics`
/// listener (spec Decision 2).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrometheusConfig {
    /// The address `/metrics` is served on. It must not overlap the main
    /// `listen` address ([`PrometheusConfig::overlaps`]).
    pub listen: SocketAddr,
}

impl PrometheusConfig {
    /// Whether this listener and `main` would claim the same socket: the
    /// same address, or the same port when either address is unspecified
    /// (`0.0.0.0` or `::`, which accept on every interface).
    ///
    /// # Arguments
    /// * `main` - the server's own `listen` address.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "pure address comparison, called once at start-up"
        )
    )]
    pub fn overlaps(&self, main: SocketAddr) -> bool {
        self.listen == main
            || (self.listen.port() == main.port()
                && (self.listen.ip().is_unspecified() || main.ip().is_unspecified()))
    }
}

/// `[telemetry.stderr]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StderrConfig {
    /// `text` (today's lines) or `json` (one object per line, with trace ids).
    pub format: LogFormat,
    /// An `EnvFilter` directive; `RUST_LOG` overrides it.
    pub filter: Option<String>,
}

/// stderr's line format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable lines (the default).
    #[default]
    Text,
    /// One JSON object per line.
    Json,
}

/// `[telemetry.otlp]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OtlpConfig {
    /// The receiver's base URL; `/v1/traces`, `/v1/logs` and `/v1/metrics`
    /// are appended.
    pub endpoint: Option<String>,
    /// The environment variable holding `name=value,name=value` headers, or `none`.
    pub headers_env: Option<String>,
    /// The environment variable holding a Sentry DSN, instead of the two above.
    pub sentry_dsn_env: Option<String>,
    /// The OTLP span layer's filter.
    pub traces_filter: String,
    /// The OTLP log layer's filter.
    pub logs_filter: String,
    /// The share of new traces sampled (a parent's decision is kept);
    /// `None` leaves `OTEL_TRACES_SAMPLER` (or always-on) in charge.
    pub sample_ratio: Option<f64>,
    /// How often metrics are pushed.
    // `with = "humantime_serde"` reads `"60s"` instead of `{secs, nanos}`.
    #[serde(with = "humantime_serde")]
    pub export_interval: Duration,
}

impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            endpoint: None,
            headers_env: None,
            sentry_dsn_env: None,
            traces_filter: DEFAULT_OTLP_FILTER.to_string(),
            logs_filter: DEFAULT_OTLP_FILTER.to_string(),
            sample_ratio: None,
            export_interval: DEFAULT_EXPORT_INTERVAL,
        }
    }
}

impl TelemetryConfig {
    /// Checks the values serde cannot.
    ///
    /// # Returns
    /// `Ok(())`, or the problem in words, naming the key.
    ///
    /// # Errors
    /// An `[telemetry.otlp]` table that sets neither `endpoint` with
    /// `headers_env` nor `sentry_dsn_env`; both forms at once; an endpoint
    /// that is not an `http` or `https` URL with a host and no userinfo,
    /// query or fragment; a `sample_ratio` outside `0.0..=1.0`; a zero
    /// `export_interval`; or a filter that does not parse.
    pub fn validate(&self) -> Result<(), String> {
        check_filter("telemetry.stderr.filter", self.stderr.filter.as_deref())?;
        let Some(otlp) = &self.otlp else {
            return Ok(());
        };
        match (&otlp.endpoint, &otlp.headers_env, &otlp.sentry_dsn_env) {
            (Some(endpoint), Some(_), None) => check_endpoint(endpoint)?,
            (None, None, Some(_)) => {}
            (None, None, None) => {
                return Err(
                    "telemetry.otlp needs endpoint and headers_env, or sentry_dsn_env".to_string(),
                )
            }
            (_, _, Some(_)) => {
                return Err(
                    "telemetry.otlp.sentry_dsn_env cannot be combined with endpoint or headers_env"
                        .to_string(),
                )
            }
            (Some(_), None, None) | (None, Some(_), None) => {
                return Err(
                    "telemetry.otlp.endpoint and headers_env must be set together \
                     (headers_env = \"NAME\" names the variable, or \"none\" for no headers)"
                        .to_string(),
                )
            }
        }
        if let Some(ratio) = otlp.sample_ratio {
            if !(0.0..=1.0).contains(&ratio) {
                return Err(format!(
                    "telemetry.otlp.sample_ratio must be between 0 and 1, not {ratio}"
                ));
            }
        }
        if otlp.export_interval.is_zero() {
            return Err("telemetry.otlp.export_interval must be more than zero".to_string());
        }
        check_filter("telemetry.otlp.traces_filter", Some(&otlp.traces_filter))?;
        check_filter("telemetry.otlp.logs_filter", Some(&otlp.logs_filter))
    }
}

/// Checks an endpoint is an `http` or `https` URL with a host, and no
/// userinfo, query or fragment (the exporters append `/v1/<signal>`).
fn check_endpoint(endpoint: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|_| "telemetry.otlp.endpoint is not a URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("telemetry.otlp.endpoint must be an http or https URL".to_string());
    }
    if url.host_str().is_none() {
        return Err("telemetry.otlp.endpoint has no host".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("telemetry.otlp.endpoint must have no query or fragment".to_string());
    }
    // Userinfo would end up in the exporter's request log lines.
    if !url.username().is_empty() || url.password().is_some() {
        return Err("telemetry.otlp.endpoint must not carry a user name or password".to_string());
    }
    Ok(())
}

/// Parses `directives` as an `EnvFilter`, naming `key` in the error.
fn check_filter(key: &str, directives: Option<&str>) -> Result<(), String> {
    match directives {
        Some(text) => parse_filter(text)
            .map(drop)
            .map_err(|error| format!("{key} is not a valid filter: {error}")),
        None => Ok(()),
    }
}

/// Parses filter directives, trimming each comma-separated component first:
/// tracing-subscriber 0.3.23 panics on a component with leading whitespace
/// before a multibyte character.
///
/// # Arguments
/// * `text` - directives such as `warn,okf_mcp=debug`.
///
/// # Returns
/// The filter.
///
/// # Errors
/// The parser's message.
pub(crate) fn parse_filter(text: &str) -> Result<EnvFilter, String> {
    let trimmed = text
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    EnvFilter::try_new(trimmed).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn absent_table_means_text_stderr_and_no_otlp() {
        let config = Config::from_toml("").expect("parses");
        assert_eq!(config.telemetry, TelemetryConfig::default());
        assert!(config.telemetry.otlp.is_none());
        assert_eq!(config.telemetry.stderr.format, LogFormat::Text);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn full_table_parses_and_validates() {
        let config = Config::from_toml(
            "[telemetry]\nservice_name = \"okf\"\nenvironment = \"prod\"\n\
             [telemetry.stderr]\nformat = \"json\"\nfilter = \"info\"\n\
             [telemetry.otlp]\nendpoint = \"https://otlp.example.com\"\n\
             headers_env = \"OTLP_HEADERS\"\ntraces_filter = \"debug\"\n\
             sample_ratio = 0.25\nexport_interval = \"30s\"\n",
        )
        .expect("parses");
        config.validate().expect("valid");
        assert_eq!(config.telemetry.stderr.format, LogFormat::Json);
        let otlp = config.telemetry.otlp.expect("otlp");
        assert_eq!(otlp.export_interval, Duration::from_secs(30));
        assert_eq!(otlp.logs_filter, DEFAULT_OTLP_FILTER);
    }

    fn refused(toml: &str) -> String {
        Config::from_toml(toml)
            .expect("parses")
            .validate()
            .expect_err("refused")
    }

    #[test]
    fn endpoint_without_headers_env_is_refused() {
        let error = refused("[telemetry.otlp]\nendpoint = \"https://otlp.example.com\"\n");
        assert!(
            error.contains("telemetry.otlp.endpoint and headers_env"),
            "{error}"
        );
    }

    #[test]
    fn empty_otlp_table_is_refused() {
        let error = refused("[telemetry.otlp]\n");
        assert!(error.contains("needs endpoint and headers_env"), "{error}");
    }

    #[test]
    fn sentry_dsn_env_excludes_endpoint() {
        let error = refused(
            "[telemetry.otlp]\nendpoint = \"https://otlp.example.com\"\n\
             headers_env = \"H\"\nsentry_dsn_env = \"DSN\"\n",
        );
        assert!(
            error.contains("sentry_dsn_env") && error.contains("endpoint"),
            "{error}"
        );
        // The DSN alone is fine.
        Config::from_toml("[telemetry.otlp]\nsentry_dsn_env = \"DSN\"\n")
            .expect("parses")
            .validate()
            .expect("valid");
    }

    #[test]
    fn unknown_key_in_telemetry_is_refused() {
        assert!(Config::from_toml("[telemetry.otlp]\nheader_env = \"X\"\n").is_err());
        assert!(Config::from_toml("[telemetry]\nservice = \"X\"\n").is_err());
        assert!(Config::from_toml("[telemetry.stderr]\nformt = \"json\"\n").is_err());
    }

    #[test]
    fn sample_ratio_out_of_range_is_refused() {
        for ratio in ["1.5", "-0.1", "nan"] {
            let error = refused(&format!(
                "[telemetry.otlp]\nsentry_dsn_env = \"DSN\"\nsample_ratio = {ratio}\n"
            ));
            assert!(error.contains("sample_ratio"), "{ratio}: {error}");
        }
    }

    #[test]
    fn zero_export_interval_is_refused() {
        let error =
            refused("[telemetry.otlp]\nsentry_dsn_env = \"DSN\"\nexport_interval = \"0s\"\n");
        assert!(error.contains("export_interval"), "{error}");
    }

    #[test]
    fn endpoint_with_query_or_non_http_scheme_is_refused() {
        for endpoint in [
            "ftp://otlp.example.com",
            "https://otlp.example.com?x=1",
            "https://otlp.example.com#f",
            "https://alice:secret@otlp.example.com",
            "not a url",
        ] {
            let error = refused(&format!(
                "[telemetry.otlp]\nendpoint = \"{endpoint}\"\nheaders_env = \"H\"\n"
            ));
            assert!(
                error.contains("telemetry.otlp.endpoint"),
                "{endpoint}: {error}"
            );
            assert!(!error.contains("secret"), "{error}");
        }
    }

    #[test]
    fn filters_must_parse() {
        let error = refused("[telemetry.otlp]\nsentry_dsn_env = \"DSN\"\ntraces_filter = \"=[\"\n");
        assert!(error.contains("telemetry.otlp.traces_filter"), "{error}");
        let error = refused("[telemetry.otlp]\nsentry_dsn_env = \"DSN\"\nlogs_filter = \"=[\"\n");
        assert!(error.contains("telemetry.otlp.logs_filter"), "{error}");
        let error = refused("[telemetry.stderr]\nfilter = \"=[\"\n");
        assert!(error.contains("telemetry.stderr.filter"), "{error}");
    }

    #[test]
    fn filter_components_are_trimmed_before_parsing() {
        // A leading space before a multibyte character panics tracing-subscriber
        // 0.3.23 when parsed untrimmed.
        assert!(parse_filter("warn, okf_mcp=info").is_ok());
        let _ = parse_filter("warn,\u{a0}\u{e9}=info");
    }

    #[test]
    fn prometheus_listen_parses_and_a_distinct_port_validates() {
        let config = Config::from_toml(
            "listen = \"0.0.0.0:8080\"\n[telemetry.prometheus]\nlisten = \"0.0.0.0:9090\"\n",
        )
        .expect("parses");
        config.validate().expect("valid");
        assert_eq!(
            config.telemetry.prometheus,
            Some(PrometheusConfig {
                listen: "0.0.0.0:9090".parse().expect("address"),
            })
        );
        assert!(
            Config::from_toml("[telemetry.prometheus]\nlisten = \"127.0.0.1:9090\"\n")
                .expect("parses")
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn prometheus_listen_that_overlaps_listen_is_refused() {
        // The same address; the same port with the main address unspecified;
        // the same port with the metrics address unspecified.
        for (main, metrics) in [
            ("127.0.0.1:8080", "127.0.0.1:8080"),
            ("0.0.0.0:8080", "127.0.0.1:8080"),
            ("127.0.0.1:8080", "[::]:8080"),
        ] {
            let error = refused(&format!(
                "listen = \"{main}\"\n[telemetry.prometheus]\nlisten = \"{metrics}\"\n"
            ));
            assert!(
                error.contains("telemetry.prometheus.listen overlaps listen"),
                "{main} vs {metrics}: {error}"
            );
        }
    }

    #[test]
    fn prometheus_table_rejects_unknown_keys_and_a_missing_listen() {
        assert!(Config::from_toml("[telemetry.prometheus]\nlistn = \"127.0.0.1:9090\"\n").is_err());
        assert!(Config::from_toml("[telemetry.prometheus]\n").is_err());
    }
}

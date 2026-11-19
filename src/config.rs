//! The configuration file (TOML; the design's "Configuration"). Each
//! milestone reads the fields it uses (M7 added `time_server`). Unknown keys
//! are ignored, so one file serves every version of the server.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::Path;
use std::time::Duration;

// `Uri` and `Authority` are the `http` crate's parsed URL and `host[:port]`
// types, re-exported by axum; the MCP `Host` check parses `allowed_hosts`
// with the same `Authority`.
use axum::http::uri::{Authority, Uri};
use bytesize::ByteSize;
// lettre's parsed `Name <address>`; `mail.from` must parse as one.
use lettre::message::Mailbox;
use serde::{Deserialize, Deserializer};

use crate::store::catalogs;

use crate::error::OkfError;
use crate::telemetry::config::TelemetryConfig;

/// The default import limit: 64 MiB, uncompressed.
pub const DEFAULT_MAX_IMPORT_BYTES: u64 = 64 * 1024 * 1024;

/// The default listen address: every IPv4 interface, port 8080.
pub const DEFAULT_LISTEN: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8080));

/// rmcp's default `Host` allow-list (loopback only): right for a local run,
/// wrong behind an ingress, where the config must name the public host.
pub const DEFAULT_ALLOWED_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// How often the server looks for changes made outside it.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// How long an MCP session may sit idle before it is closed.
pub const DEFAULT_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// The default for `worker_shutdown_grace`: how long `serve` waits for the
/// mail worker to finish the mail in hand once it is told to stop. It is the
/// longest a provider may take to answer a send (30 s for Postmark and SES).
pub const DEFAULT_WORKER_SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

/// The default `get_concept` outline threshold: bodies over 20,000 characters.
pub const DEFAULT_CONCEPT_OUTLINE_CHARS: usize = 20_000;

/// The default database pool size.
pub const DEFAULT_DATABASE_MAX_CONNECTIONS: u32 = 16;

/// The default number of changeset overlays kept in memory.
pub const DEFAULT_OVERLAY_CACHE_SIZE: usize = 64;

/// The default lifetime of an OAuth access token.
pub const DEFAULT_ACCESS_TOKEN_TTL: Duration = Duration::from_secs(60 * 60);

/// The default lifetime of an OAuth refresh token (restarted by each rotation).
pub const DEFAULT_REFRESH_TOKEN_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The default idle period after which the poll loop revokes an OAuth grant
/// that holds no live token.
pub const DEFAULT_IDLE_GRANT_REVOKE_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The default cap on a sign-in code's `--expires`.
pub const DEFAULT_CODE_MAX_EXPIRY: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The default number of live self-issued keys per user.
pub const DEFAULT_MAX_KEYS_PER_USER: u32 = 20;

/// The most `max_keys_per_user` may be set to.
pub const MAX_KEYS_PER_USER_LIMIT: u32 = 1000;

/// The default console session idle timeout.
pub const DEFAULT_CONSOLE_SESSION_IDLE: Duration = Duration::from_secs(12 * 60 * 60);

/// The default console session absolute lifetime.
pub const DEFAULT_CONSOLE_SESSION_ABSOLUTE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The default lifetime of a setup link.
pub const DEFAULT_SETUP_LINK_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// The default lifetime of a password-reset link.
pub const DEFAULT_RESET_LINK_TTL: Duration = Duration::from_secs(60 * 60);

/// The `[oauth]` section: used only when `public_url` is set.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct OAuthConfig {
    /// How long an access token is accepted (`"1h"`).
    #[serde(with = "humantime_serde")]
    pub access_token_ttl: Duration,
    /// How long a refresh token is accepted after it is issued (`"30d"`);
    /// each rotation issues a new one, so the lifetime slides.
    #[serde(with = "humantime_serde")]
    pub refresh_token_ttl: Duration,
    /// How long an OAuth grant may go without a token being issued for it
    /// before the poll loop revokes it (`"30d"`). A grant that still holds
    /// a live token is never revoked, whatever this says.
    #[serde(with = "humantime_serde")]
    pub idle_grant_revoke_after: Duration,
    /// The longest `okf-mcp oauth code create --expires` accepts (`"7d"`).
    #[serde(with = "humantime_serde")]
    pub code_max_expiry: Duration,
    /// Whether `POST /oauth/register` (RFC 7591) is offered.
    pub dcr_enabled: bool,
    /// Lets a Client ID Metadata Document live on a private or loopback
    /// address. Only for test rigs: it opens the fetcher to the internal network.
    pub cimd_allow_private: bool,
}

impl Default for OAuthConfig {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
        )
    )]
    fn default() -> Self {
        Self {
            access_token_ttl: DEFAULT_ACCESS_TOKEN_TTL,
            refresh_token_ttl: DEFAULT_REFRESH_TOKEN_TTL,
            idle_grant_revoke_after: DEFAULT_IDLE_GRANT_REVOKE_AFTER,
            code_max_expiry: DEFAULT_CODE_MAX_EXPIRY,
            dcr_enabled: true,
            cimd_allow_private: false,
        }
    }
}

/// The `[auth]` section: the defaults for the console's sign-in policy.
/// The policy keys can be overridden at run time by `server_settings`
/// (`store::settings`); `trusted_proxy_header` is config-only.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// Whether `/signin` offers email and password.
    pub password_sign_in: bool,
    /// Live self-issued keys a user may hold.
    pub max_keys_per_user: u32,
    /// The longest expiry a self-issued key may have; `None` is no limit.
    // humantime_serde reads `Option<Duration>` too: a missing key is `None`.
    #[serde(with = "humantime_serde")]
    pub max_key_lifetime: Option<Duration>,
    /// How long a console session may sit idle (`"12h"`).
    #[serde(with = "humantime_serde")]
    pub console_session_idle: Duration,
    /// How long a console session lasts at most (`"7d"`).
    #[serde(with = "humantime_serde")]
    pub console_session_absolute: Duration,
    /// How long a setup link works (`"24h"`).
    #[serde(with = "humantime_serde")]
    pub setup_link_ttl: Duration,
    /// How long a password-reset link works (`"1h"`).
    #[serde(with = "humantime_serde")]
    pub reset_link_ttl: Duration,
    /// The header whose last hop is the client's address behind a proxy
    /// (`"x-forwarded-for"`); unset means the peer address (Decision 7).
    pub trusted_proxy_header: Option<String>,
    /// The OIDC providers (`[[auth.oidc]]`); none by default.
    pub oidc: Vec<OidcProviderConfig>,
}

/// One `[[auth.oidc]]` provider (spec "OIDC"). The client secret is read
/// from the environment variable `client_secret_env` at start-up, never
/// from this file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
// A misspelt key (or an inline `client_secret`) is an error, not ignored.
#[serde(deny_unknown_fields)]
pub struct OidcProviderConfig {
    /// Stable id: stored in `user_identities.provider` and part of the
    /// redirect URI (`a-z`, `0-9`, `-`; at most 32).
    pub id: String,
    /// The sign-in button's text ("Sign in with `label`").
    pub label: String,
    /// The issuer URL; discovery reads `<issuer>/.well-known/openid-configuration`.
    pub issuer: String,
    /// The client id registered with the provider.
    pub client_id: String,
    /// The environment variable holding the client secret.
    pub client_secret_env: String,
    /// The scopes asked for (`openid` is always sent).
    #[serde(default = "default_oidc_scopes")]
    pub scopes: Vec<String>,
}

/// Whether `id` is a well-formed `[[auth.oidc]]` id: 1 to 32 of `a-z`,
/// `0-9` and `-`. Config validation requires it, and unlinking uses it to
/// answer a malformed provider (a NUL, which Postgres refuses, included)
/// as unknown before any query.
///
/// # Arguments
/// * `id` - the candidate id.
///
/// # Returns
/// `true` when it is well formed.
#[tracing::instrument(level = "debug", skip_all)]
pub(crate) fn is_provider_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// `["openid", "email", "profile"]`.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
    )
)]
fn default_oidc_scopes() -> Vec<String> {
    Vec::from(["openid", "email", "profile"].map(String::from))
}

impl Default for AuthConfig {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
        )
    )]
    fn default() -> Self {
        Self {
            password_sign_in: true,
            max_keys_per_user: DEFAULT_MAX_KEYS_PER_USER,
            max_key_lifetime: None,
            console_session_idle: DEFAULT_CONSOLE_SESSION_IDLE,
            console_session_absolute: DEFAULT_CONSOLE_SESSION_ABSOLUTE,
            setup_link_ttl: DEFAULT_SETUP_LINK_TTL,
            reset_link_ttl: DEFAULT_RESET_LINK_TTL,
            trusted_proxy_header: None,
            oidc: Vec::new(),
        }
    }
}

impl AuthConfig {
    /// Checks the values serde cannot.
    ///
    /// # Returns
    /// `Ok(())`, or the problem in words.
    ///
    /// # Errors
    /// A zero or out-of-range duration, `max_keys_per_user` above
    /// [`MAX_KEYS_PER_USER_LIMIT`], or a `trusted_proxy_header` that is not
    /// an HTTP header name.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
        )
    )]
    fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("auth.console_session_idle", self.console_session_idle),
            (
                "auth.console_session_absolute",
                self.console_session_absolute,
            ),
            ("auth.setup_link_ttl", self.setup_link_ttl),
            ("auth.reset_link_ttl", self.reset_link_ttl),
        ] {
            check_auth_duration(name, value)?;
        }
        if let Some(lifetime) = self.max_key_lifetime {
            check_auth_duration("auth.max_key_lifetime", lifetime)?;
        }
        if self.max_keys_per_user > MAX_KEYS_PER_USER_LIMIT {
            return Err(format!(
                "auth.max_keys_per_user must be at most {MAX_KEYS_PER_USER_LIMIT}"
            ));
        }
        if let Some(header) = self.trusted_proxy_header.as_deref() {
            // The same parser axum uses for request headers; it refuses an
            // empty name too.
            if axum::http::HeaderName::from_bytes(header.as_bytes()).is_err() {
                return Err(format!(
                    "auth.trusted_proxy_header {header:?} is not an HTTP header name"
                ));
            }
        }
        Ok(())
    }
}

/// The longest `[auth]` or `[oauth]` lifetime the config and settings
/// accept: ten years, in humantime's own year (365.25 days), so
/// `"10years"` is accepted exactly. It is far inside `DateTime<Utc>`'s
/// calendar range: added to any real `now` it is always a valid
/// timestamp, so a lifetime that passed the check can never make
/// `clock::after` fail.
pub const MAX_AUTH_DURATION: Duration = Duration::from_secs(10 * 31_557_600);

/// Checks one lifetime setting: above zero and at most
/// [`MAX_AUTH_DURATION`]. Shared by the config file (`[oauth]` and
/// `[auth]`) and `store::settings` (both when a value is set and when a
/// stored one is loaded), so a value is accepted or refused the same way
/// everywhere.
///
/// # Arguments
/// * `name` - the key, for the message.
/// * `value` - the duration.
///
/// # Returns
/// `Ok(())`, or the problem in words.
///
/// # Errors
/// A zero duration, or one longer than [`MAX_AUTH_DURATION`].
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
    )
)]
pub(crate) fn check_auth_duration(name: &str, value: Duration) -> Result<(), String> {
    if value.is_zero() {
        return Err(format!("{name} must be more than zero"));
    }
    if value > MAX_AUTH_DURATION {
        return Err(format!("{name} must be at most 10 years"));
    }
    Ok(())
}

/// The default From address: a placeholder that is valid, so a server
/// without `[mail]` starts (with the log provider, it sends nothing).
pub const DEFAULT_MAIL_FROM: &str = "okf <okf@localhost>";

/// The default SMTP port: submission with STARTTLS.
pub const DEFAULT_SMTP_PORT: u16 = 587;

/// The default Postmark message stream (its transactional one).
pub const DEFAULT_POSTMARK_STREAM: &str = "outbound";

/// Which [`crate::mail::Mailer`] sends mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailProvider {
    /// An SMTP relay (`[mail.smtp]`).
    Smtp,
    /// Postmark's HTTP API (`[mail.postmark]`).
    Postmark,
    /// Amazon SES through the AWS SDK (`[mail.ses]`, optional), with the
    /// standard AWS credential chain (EKS Pod Identity among it).
    Ses,
    /// Writes each message, links included, to the log at `info`. For
    /// development and tests only.
    #[default]
    Log,
}

impl MailProvider {
    /// The config spelling.
    ///
    /// # Returns
    /// `"smtp"`, `"postmark"`, `"ses"` or `"log"`, as `[mail] provider`
    /// takes it.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "a pure match returning a closed value: a span would record nothing new"
        )
    )]
    pub fn as_str(self) -> &'static str {
        match self {
            MailProvider::Smtp => "smtp",
            MailProvider::Postmark => "postmark",
            MailProvider::Ses => "ses",
            MailProvider::Log => "log",
        }
    }
}

/// `[mail.smtp]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SmtpConfig {
    /// The relay's host name.
    pub host: String,
    /// Its port (587 for STARTTLS, 465 for implicit TLS).
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    /// `true`: STARTTLS, required (the session never continues in clear).
    /// `false`: TLS from the first byte. There is no plaintext mode.
    #[serde(default = "default_true")]
    pub starttls: bool,
    /// The user to authenticate as, if the relay wants one.
    #[serde(default)]
    pub username: Option<String>,
    /// The environment variable holding the password (required with
    /// `username`).
    #[serde(default)]
    pub password_env: Option<String>,
}

/// `[mail.postmark]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PostmarkConfig {
    /// The environment variable holding the server token.
    pub token_env: String,
    /// The message stream (`"outbound"`, Postmark's default transactional stream).
    #[serde(default = "default_message_stream")]
    pub message_stream: String,
}

/// `[mail.ses]`. Credentials are never here: the AWS SDK's standard chain
/// finds them (environment, profile, web identity, or the container
/// provider that EKS Pod Identity configures).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SesConfig {
    /// The AWS region to send in (`"us-east-1"`). Unset: the SDK's own
    /// resolution (`AWS_REGION`, then the profile, then instance metadata).
    pub region: Option<String>,
}

// `serde(default = "...")` takes a function's name, not a value.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
    )
)]
fn default_smtp_port() -> u16 {
    DEFAULT_SMTP_PORT
}

#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
    )
)]
fn default_true() -> bool {
    true
}

#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
    )
)]
fn default_message_stream() -> String {
    DEFAULT_POSTMARK_STREAM.to_string()
}

/// The `[mail]` section. Secrets are not here: only the names of the
/// environment variables that hold them, read when the server starts.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct MailConfig {
    /// Which provider sends.
    pub provider: MailProvider,
    /// The From address (`"okf <okf@example.com>"`).
    pub from: String,
    /// Deprecated and ignored: every `serve` runs the mail worker, so a
    /// Deployment of several replicas needs no per-pod mail setting (issue
    /// #458). The key still parses, so a configuration that has it keeps
    /// starting; [`Config::deprecation_warnings`] names it. The worker's
    /// pool is shared with the housekeeping worker: up to 3 connections
    /// beside `database_max_connections`.
    pub worker: Option<bool>,
    /// The SMTP relay, for `provider = "smtp"`.
    pub smtp: Option<SmtpConfig>,
    /// Postmark, for `provider = "postmark"`.
    pub postmark: Option<PostmarkConfig>,
    /// SES, for `provider = "ses"` (optional: every key has a default).
    pub ses: Option<SesConfig>,
}

impl Default for MailConfig {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
        )
    )]
    fn default() -> Self {
        Self {
            provider: MailProvider::Log,
            from: DEFAULT_MAIL_FROM.to_string(),
            worker: None,
            smtp: None,
            postmark: None,
            ses: None,
        }
    }
}

impl MailConfig {
    /// Checks what serde cannot: that the chosen provider is configured,
    /// and that `from` is one address.
    ///
    /// # Returns
    /// `Ok(())`, or the problem in words.
    ///
    /// # Errors
    /// A `from` that is not one mailbox; `provider = "smtp"` without a
    /// `[mail.smtp]` table, with a blank host, port 0, or a `username`
    /// without `password_env` (or the reverse); `provider = "postmark"`
    /// without a `[mail.postmark]` table, or with a blank `token_env` or
    /// `message_stream`; `provider = "ses"` with a blank `region`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
        )
    )]
    fn validate(&self) -> Result<(), String> {
        self.from
            .parse::<Mailbox>()
            .map_err(|error| format!("mail.from {:?} is not an address: {error}", self.from))?;
        match self.provider {
            MailProvider::Log => Ok(()),
            MailProvider::Smtp => {
                // `let ... else`: bind the `Some`, or leave the function.
                let Some(smtp) = &self.smtp else {
                    return Err("mail.provider = \"smtp\" needs a [mail.smtp] table".to_string());
                };
                if smtp.host.trim().is_empty() {
                    return Err("mail.smtp.host is blank".to_string());
                }
                if smtp.port == 0 {
                    return Err("mail.smtp.port must be more than zero".to_string());
                }
                if smtp.username.is_some() != smtp.password_env.is_some() {
                    return Err(
                        "mail.smtp.username and mail.smtp.password_env go together".to_string()
                    );
                }
                if smtp
                    .password_env
                    .as_deref()
                    .is_some_and(|name| name.trim().is_empty())
                {
                    return Err("mail.smtp.password_env is blank".to_string());
                }
                Ok(())
            }
            MailProvider::Postmark => {
                let Some(postmark) = &self.postmark else {
                    return Err(
                        "mail.provider = \"postmark\" needs a [mail.postmark] table".to_string()
                    );
                };
                if postmark.token_env.trim().is_empty() {
                    return Err("mail.postmark.token_env is blank".to_string());
                }
                if postmark.message_stream.trim().is_empty() {
                    return Err("mail.postmark.message_stream is blank".to_string());
                }
                Ok(())
            }
            MailProvider::Ses => {
                // `and_then` reaches through both `Option`s: the table, then
                // its key. A missing region is fine (the SDK resolves one at
                // start-up); a blank one is a mistake.
                if self
                    .ses
                    .as_ref()
                    .and_then(|ses| ses.region.as_deref())
                    .is_some_and(|region| region.trim().is_empty())
                {
                    return Err("mail.ses.region is blank".to_string());
                }
                Ok(())
            }
        }
    }
}

/// The longest `[redis] key_prefix` accepted, in bytes.
const MAX_REDIS_KEY_PREFIX_BYTES: usize = 64;

#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "serde default for a config key, read before the subscriber exists"
    )
)]
fn default_key_prefix() -> String {
    "okf:".to_string()
}

/// Which Redis client `[redis]` uses (spec Decision 20).
// `rename_all = "lowercase"` makes the TOML spelling `"standalone"` or
// `"cluster"`; anything else is a parse error (Python: an Enum lookup).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RedisMode {
    /// One Redis or Valkey primary (`ConnectionManager`).
    #[default]
    Standalone,
    /// A cluster, including ElastiCache Serverless (`ClusterClient`).
    Cluster,
}

/// The `[redis]` table (spec Decision 1). Absent: every short-lived state stays
/// in memory and one replica is required. Present: `serve` keeps it in Redis.
// `deny_unknown_fields` makes a misspelt key (or a `url = ...` that would put a
// password in the file) a parse error instead of being ignored silently.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedisConfig {
    /// The environment variable holding the Redis URL (`redis://` or
    /// `rediss://`; it may carry a password, so it never goes in the file).
    pub url_env: String,
    /// Prepended to every key and channel, so one Redis can serve several
    /// deployments. Default `"okf:"`.
    #[serde(default = "default_key_prefix")]
    pub key_prefix: String,
    /// `"standalone"` (default) or `"cluster"` (ElastiCache Serverless and
    /// other clusters). Every key and channel carries one hash tag in both
    /// modes (spec Decision 20).
    #[serde(default)]
    pub mode: RedisMode,
}

impl RedisConfig {
    /// Checks the variable name and the key prefix.
    ///
    /// # Returns
    /// `Ok(())` when both are usable.
    ///
    /// # Errors
    /// A message naming the bad key: `url_env` that is empty or has a
    /// character outside `[A-Za-z0-9_]`; `key_prefix` that is empty, over 64
    /// bytes, or holds whitespace, a control character or a brace (the prefix
    /// goes inside the hash tag `{<prefix>}`).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "config check run once at start-up, like `Config::validate`"
        )
    )]
    pub fn validate(&self) -> Result<(), String> {
        if self.url_env.is_empty()
            || !self
                .url_env
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(
                "redis.url_env must name an environment variable (letters, digits and _)"
                    .to_string(),
            );
        }
        let prefix = &self.key_prefix;
        if prefix.is_empty()
            || prefix.len() > MAX_REDIS_KEY_PREFIX_BYTES
            || prefix
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == '{' || c == '}')
        {
            return Err(format!(
                "redis.key_prefix must be 1 to {MAX_REDIS_KEY_PREFIX_BYTES} bytes with no whitespace, control characters or braces"
            ));
        }
        Ok(())
    }
}

/// Settings read from the configuration file.
// `#[serde(default)]` fills every missing key from `Default::default()`.
// serde ignores keys the struct does not name unless told otherwise
// (`deny_unknown_fields`), which is what lets later milestones' keys through.
// Not `Eq`: `[telemetry.otlp] sample_ratio` is an `f64`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The largest uncompressed total an import may expand to (`"64MiB"`, or
    /// a number of bytes). `MiB` is binary; `MB` and `M` are decimal. For a
    /// tarball it counts the whole decompressed tar stream (headers and
    /// 512-byte padding included); for a directory, the files' contents.
    /// The import route caps the request body at this size plus a little
    /// slack for gzip (`body_limit`).
    pub max_import_bytes: ByteSize,
    /// The address `serve` listens on (`"0.0.0.0:8080"`); `--listen` overrides it.
    pub listen: SocketAddr,
    /// The `Host` names (optionally `host:port`) MCP requests may carry, the
    /// DNS-rebinding check. Behind an ingress, the public hostname.
    pub allowed_hosts: Vec<String>,
    /// The catalog a key with no default of its own reads and writes (design
    /// "Catalog selection", step 4).
    pub default_catalog: Option<String>,
    /// How often to look for imports, catalog and key changes made by the
    /// subcommands (`"10s"`).
    // `with = "humantime_serde"` parses the field through that module's
    // functions instead of `Duration`'s own serde impl (which wants
    // `{secs, nanos}`).
    #[serde(with = "humantime_serde")]
    pub poll_interval: Duration,
    /// How long an MCP session may be idle before it is closed (`"2h"`).
    #[serde(with = "humantime_serde")]
    pub session_idle_timeout: Duration,
    /// How long `serve` waits, once told to stop, for the mail worker to
    /// finish the mail in hand (`"30s"`). Match the pod's
    /// `terminationGracePeriodSeconds` to it, plus a few seconds. Past it
    /// `serve` logs and returns, and apalis hands the unfinished job to
    /// another replica after the orphan timeout.
    #[serde(with = "humantime_serde")]
    pub worker_shutdown_grace: Duration,
    /// The NTP server the clock check measures against (`host`, `host:port`
    /// or an address); none means no check (design "Time").
    pub time_server: Option<String>,
    /// Review cadence overrides keyed by catalog name.
    #[serde(deserialize_with = "deserialize_maintenance_intervals")]
    pub maintenance_review_intervals: BTreeMap<String, Duration>,
    /// `get_concept` returns an outline instead of a body longer than this
    /// many characters (Unicode code points), unless asked for `full`.
    pub concept_outline_chars: usize,
    /// The most database connections one process opens (every command,
    /// `serve` included). A write holds one for its transaction, and a read
    /// that builds an overlay holds one to read the changeset's entries.
    /// `serve`'s workers do not count against it: they share one pool of up
    /// to `mail::queue::WORKER_POOL_SIZE` (3) connections of their own.
    pub database_max_connections: u32,
    /// The most changeset overlays kept in memory; past it, the least
    /// recently used one is dropped (and rebuilt if read again).
    pub overlay_cache_size: usize,
    /// The externally visible origin (`"https://okf.example.com"`). Setting
    /// it turns OAuth on: it is the issuer, and `<public_url>/mcp` is the
    /// resource tokens are issued for. Its host must be in `allowed_hosts`.
    pub public_url: Option<String>,
    /// The OAuth settings (`[oauth]`).
    pub oauth: OAuthConfig,
    /// The console sign-in policy defaults (`[auth]`).
    pub auth: AuthConfig,
    /// Outgoing mail (`[mail]`).
    pub mail: MailConfig,
    /// Logs, traces and metrics (`[telemetry]`).
    pub telemetry: TelemetryConfig,
    /// Shared state for several `serve` replicas (`[redis]`); `None` keeps
    /// everything in memory.
    pub redis: Option<RedisConfig>,
}

impl Default for Config {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
        )
    )]
    fn default() -> Self {
        Self {
            max_import_bytes: ByteSize::b(DEFAULT_MAX_IMPORT_BYTES),
            listen: DEFAULT_LISTEN,
            // `Vec::from` takes ownership of the array (after `.map` turns each
            // `&str` into an owned `String`) without copying it a second time.
            allowed_hosts: Vec::from(DEFAULT_ALLOWED_HOSTS.map(String::from)),
            default_catalog: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
            session_idle_timeout: DEFAULT_SESSION_IDLE_TIMEOUT,
            worker_shutdown_grace: DEFAULT_WORKER_SHUTDOWN_GRACE,
            time_server: None,
            maintenance_review_intervals: BTreeMap::new(),
            concept_outline_chars: DEFAULT_CONCEPT_OUTLINE_CHARS,
            database_max_connections: DEFAULT_DATABASE_MAX_CONNECTIONS,
            overlay_cache_size: DEFAULT_OVERLAY_CACHE_SIZE,
            public_url: None,
            oauth: OAuthConfig::default(),
            auth: AuthConfig::default(),
            mail: MailConfig::default(),
            telemetry: TelemetryConfig::default(),
            redis: None,
        }
    }
}

#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
    )
)]
fn deserialize_maintenance_intervals<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct HumanDuration(#[serde(with = "humantime_serde")] Duration);

    let values = BTreeMap::<String, HumanDuration>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .map(|(name, value)| (name, value.0))
        .collect())
}

impl Config {
    /// Review cadence for a catalog, defaulting to 30 days.
    #[must_use]
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn maintenance_review_interval(&self, catalog: &str) -> chrono::Duration {
        let interval = self
            .maintenance_review_intervals
            .get(catalog)
            .copied()
            .unwrap_or(Duration::from_secs(2_592_000));
        chrono::Duration::from_std(interval).expect("validated maintenance interval fits chrono")
    }

    /// Parses a configuration file's text. It does not call
    /// [`Config::validate`]; [`Config::load`] does, so a caller that parses
    /// text itself must validate the result before serving with it.
    ///
    /// # Arguments
    /// * `text` - the file's contents.
    ///
    /// # Returns
    /// The settings, with defaults for every key the text leaves out.
    ///
    /// # Errors
    /// The TOML parser's error if the text is not valid TOML or a value has
    /// the wrong type.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
        )
    )]
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Settings that are read but no longer do anything. `Config::load` runs
    /// before the log subscriber exists, so it cannot log them; `serve` calls
    /// this once the subscriber is up and logs each message as a warning.
    ///
    /// # Returns
    /// One message per deprecated setting present in the file (empty when
    /// there is none).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "pure check over the parsed settings, called once at start-up"
        )
    )]
    pub fn deprecation_warnings(&self) -> Vec<&'static str> {
        let mut warnings = Vec::new();
        if self.mail.worker.is_some() {
            warnings.push("mail.worker is deprecated and ignored: every serve runs the worker");
        }
        warnings
    }

    /// Checks the values serde cannot: ones that would break the server.
    ///
    /// # Returns
    /// `Ok(())`, or the problem in words.
    ///
    /// # Errors
    /// A zero `poll_interval` (tokio's interval panics on zero) or
    /// `session_idle_timeout`, an empty `allowed_hosts` (which rmcp reads as
    /// "accept any `Host`"), or a blank entry in it (which no `Host` matches,
    /// so every request would get 403 with no hint why), or a blank
    /// `time_server` or one with surrounding whitespace (which the lookup
    /// would fail on every check, seen only as a `warn`), or a zero
    /// `database_max_connections` or `overlay_cache_size`, a zero OAuth
    /// lifetime, an `[auth]` value out of range (see `AuthConfig`), a
    /// `[mail]` section that cannot send (see `MailConfig`), or a
    /// `public_url` that is not an `https` origin (`http` on
    /// a loopback host only) whose host `allowed_hosts` admits, or an
    /// `[[auth.oidc]]` provider that cannot work (see `validate_oidc`).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
        )
    )]
    pub fn validate(&self) -> Result<(), String> {
        if self.poll_interval.is_zero() {
            return Err("poll_interval must be more than zero".to_string());
        }
        if self.session_idle_timeout.is_zero() {
            return Err("session_idle_timeout must be more than zero".to_string());
        }
        if self.worker_shutdown_grace.is_zero() {
            return Err("worker_shutdown_grace must be more than zero".to_string());
        }
        if self.database_max_connections == 0 {
            return Err("database_max_connections must be more than zero".to_string());
        }
        if self.overlay_cache_size == 0 {
            return Err("overlay_cache_size must be more than zero".to_string());
        }
        if self.allowed_hosts.is_empty() {
            return Err(
                "allowed_hosts is empty; list the host names clients connect to".to_string(),
            );
        }
        // `trim` returns a borrowed slice (`&str`) without the surrounding
        // whitespace, so the check allocates nothing.
        if self.allowed_hosts.iter().any(|host| host.trim().is_empty()) {
            return Err("allowed_hosts has a blank entry; remove it or name a host".to_string());
        }
        // `if let Some(x) = ...`: Python's `if (x := ...) is not None`.
        // `as_deref` turns `&Option<String>` into `Option<&str>`.
        if let Some(server) = self.time_server.as_deref() {
            let trimmed = server.trim();
            if trimmed.is_empty() {
                return Err("time_server is blank; name the NTP server or leave it out".to_string());
            }
            if trimmed != server {
                return Err("time_server has surrounding whitespace; remove it".to_string());
            }
        }
        for (name, interval) in &self.maintenance_review_intervals {
            if !catalogs::is_valid_name(name) {
                return Err(format!(
                    "maintenance_review_intervals: invalid catalog name {name:?}"
                ));
            }
            if *interval < Duration::from_secs(86_400) {
                return Err(format!(
                    "maintenance_review_intervals.{name} must be at least 1 day"
                ));
            }
            if chrono::Duration::from_std(*interval).is_err() {
                return Err(format!(
                    "maintenance_review_intervals.{name} exceeds supported duration"
                ));
            }
        }
        for (name, value) in [
            ("oauth.access_token_ttl", self.oauth.access_token_ttl),
            ("oauth.refresh_token_ttl", self.oauth.refresh_token_ttl),
            (
                "oauth.idle_grant_revoke_after",
                self.oauth.idle_grant_revoke_after,
            ),
            ("oauth.code_max_expiry", self.oauth.code_max_expiry),
        ] {
            check_auth_duration(name, value)?;
        }
        self.auth.validate()?;
        self.mail.validate()?;
        self.telemetry.validate()?;
        if let Some(redis) = &self.redis {
            redis.validate()?;
        }
        self.validate_metrics_listen()?;
        self.validate_oidc()?;
        self.validate_public_url()
    }

    /// Checks that `[telemetry.prometheus] listen` does not claim the socket
    /// `listen` does (`serve --listen` is checked again where it applies).
    ///
    /// # Errors
    /// The two addresses overlap.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "config check run once at start-up, like `validate`"
        )
    )]
    pub fn validate_metrics_listen(&self) -> Result<(), String> {
        match &self.telemetry.prometheus {
            Some(prometheus) if prometheus.overlaps(self.listen) => {
                Err("telemetry.prometheus.listen overlaps listen".to_string())
            }
            _ => Ok(()),
        }
    }

    /// Checks `[[auth.oidc]]`: providers need `public_url` (the redirect
    /// URI's origin); each has a valid, unique id, a label, a client id and
    /// a secret variable, and an `https` issuer (`http` on loopback only)
    /// with no query or fragment.
    ///
    /// # Returns
    /// `Ok(())` without providers, or when every one is acceptable.
    ///
    /// # Errors
    /// The problem in words, naming the provider and the key.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
        )
    )]
    fn validate_oidc(&self) -> Result<(), String> {
        if self.auth.oidc.is_empty() {
            return Ok(());
        }
        if self.public_url.is_none() {
            return Err("auth.oidc needs public_url, the origin of its redirect URIs".to_string());
        }
        let mut seen = std::collections::HashSet::new();
        for provider in &self.auth.oidc {
            let id = provider.id.as_str();
            if !is_provider_id(id) {
                return Err(format!(
                    "auth.oidc id {id:?} must be 1 to 32 of a-z, 0-9 and -"
                ));
            }
            // `insert` returns `false` for a value already in the set.
            if !seen.insert(id) {
                return Err(format!("auth.oidc id {id:?} is listed twice"));
            }
            for (key, value) in [
                ("label", &provider.label),
                ("client_id", &provider.client_id),
                ("client_secret_env", &provider.client_secret_env),
            ] {
                if value.trim().is_empty() {
                    return Err(format!("auth.oidc.{id}: {key} is empty"));
                }
            }
            let issuer = url::Url::parse(&provider.issuer).map_err(|_| {
                format!("auth.oidc.{id}: issuer {:?} is not a URL", provider.issuer)
            })?;
            let loopback = issuer
                .host_str()
                .is_some_and(|host| is_loopback(&normalize_host(host)));
            let secure = issuer.scheme() == "https" || (issuer.scheme() == "http" && loopback);
            if !secure || issuer.query().is_some() || issuer.fragment().is_some() {
                return Err(format!(
                    "auth.oidc.{id}: issuer {:?} must be an https URL (http only on a \
                     loopback host), with no query or fragment",
                    provider.issuer
                ));
            }
        }
        Ok(())
    }

    /// Checks `public_url`: an `https` origin (`http` only on a loopback
    /// host, for local testing), with no user info, path or query, whose
    /// host an `allowed_hosts` entry admits under the MCP `Host` check's
    /// rule (same host; the entry's port, if it has one, equal to the URL's).
    ///
    /// # Returns
    /// `Ok(())` when `public_url` is unset or acceptable.
    ///
    /// # Errors
    /// The problem in words.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
        )
    )]
    fn validate_public_url(&self) -> Result<(), String> {
        let Some(public_url) = self.public_url.as_deref() else {
            return Ok(());
        };
        let uri: Uri = public_url
            .parse()
            .map_err(|_| format!("public_url {public_url:?} is not a URL"))?;
        // A tuple pattern matches both `Option`s at once.
        let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) else {
            return Err(format!(
                "public_url {public_url:?} needs a scheme and a host"
            ));
        };
        // `user@host` would put the user info into the issuer and every URL
        // built from it.
        if authority.as_str().contains('@') {
            return Err(format!(
                "public_url {public_url:?} must be an origin, with no user info"
            ));
        }
        if !matches!(uri.path(), "" | "/") || uri.query().is_some() {
            return Err(format!(
                "public_url {public_url:?} must be an origin, with no path or query"
            ));
        }
        let host = normalize_host(authority.host());
        let secure = scheme.eq_ignore_ascii_case("https")
            || (scheme.eq_ignore_ascii_case("http") && is_loopback(&host));
        if !secure {
            return Err(format!(
                "public_url {public_url:?} must use https (http only on a loopback host)"
            ));
        }
        let port = authority.port_u16();
        let admitted = self.allowed_hosts.iter().any(|entry| {
            // An entry that is not an authority (rmcp's fallback) is a bare host.
            let (entry_host, entry_port) = match entry.trim().parse::<Authority>() {
                Ok(parsed) => (normalize_host(parsed.host()), parsed.port_u16()),
                Err(_) => (normalize_host(entry.trim()), None),
            };
            // `is_none_or`: true for `None`, else the closure's answer.
            entry_host == host && entry_port.is_none_or(|entry_port| Some(entry_port) == port)
        });
        if !admitted {
            return Err(format!(
                "public_url's host {host:?} is not in allowed_hosts, so MCP requests to it \
                 would be refused; add it"
            ));
        }
        Ok(())
    }

    /// Reads the configuration file, or returns the defaults without one.
    ///
    /// # Arguments
    /// * `path` - the file (`--config` or `OKF_MCP_CONFIG`), if any.
    ///
    /// # Returns
    /// The file's settings, or [`Config::default`] when `path` is `None`.
    ///
    /// # Errors
    /// [`OkfError::Config`] if the file can't be read or parsed, or fails
    /// [`Config::validate`].
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
        )
    )]
    pub fn load(path: Option<&Path>) -> Result<Self, OkfError> {
        // `let ... else` is a pattern match that must diverge (here `return`)
        // when the pattern does not match: an early return for `None`.
        let Some(path) = path else {
            return Ok(Self::default());
        };
        // A closure (Python's lambda) that wraps a message with the path.
        let error = |message: String| OkfError::Config {
            path: path.display().to_string(),
            message,
        };
        let text = std::fs::read_to_string(path).map_err(|e| error(e.to_string()))?;
        let config = Self::from_toml(&text).map_err(|e| error(e.to_string()))?;
        // `map_err(error)` passes the closure itself: `validate`'s `String`
        // is already the message it wants.
        config.validate().map_err(error)?;
        Ok(config)
    }
}

/// A host as the MCP `Host` check compares it: lowercase, IPv6 brackets trimmed.
///
/// # Arguments
/// * `host` - a host as parsed from a URL or an `allowed_hosts` entry.
///
/// # Returns
/// The normalized host.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
    )
)]
fn normalize_host(host: &str) -> String {
    host.trim_matches(|c| c == '[' || c == ']')
        .to_ascii_lowercase()
}

/// Is `host` (normalized) this machine?
///
/// # Arguments
/// * `host` - a host from [`normalize_host`].
///
/// # Returns
/// `true` for `localhost` and loopback addresses of either family.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "runs before the subscriber exists (configuration, builder or start-up), so a span would be recorded nowhere"
    )
)]
fn is_loopback(host: &str) -> bool {
    // `parse::<IpAddr>()` reads either address family; `is_ok_and` is
    // Python's `try: ... except: False` around a test.
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
mod tests {

    #[test]
    fn redis_is_off_by_default_and_parses_with_defaults() {
        assert_eq!(Config::default().redis, None);
        let config = Config::from_toml("[redis]\nurl_env = \"OKF_REDIS_URL\"\n").unwrap();
        let redis = config.redis.clone().expect("a [redis] table");
        assert_eq!(redis.url_env, "OKF_REDIS_URL");
        assert_eq!(redis.key_prefix, "okf:");
        assert_eq!(redis.mode, RedisMode::Standalone);
        config.validate().unwrap();
    }

    #[test]
    fn redis_mode_parses_cluster_and_refuses_anything_else() {
        let config = Config::from_toml("[redis]\nurl_env = \"X\"\nmode = \"cluster\"\n").unwrap();
        assert_eq!(config.redis.expect("a table").mode, RedisMode::Cluster);
        let config =
            Config::from_toml("[redis]\nurl_env = \"X\"\nmode = \"standalone\"\n").unwrap();
        assert_eq!(config.redis.expect("a table").mode, RedisMode::Standalone);
        assert!(Config::from_toml("[redis]\nurl_env = \"X\"\nmode = \"sentinel\"\n").is_err());
    }

    #[test]
    fn redis_rejects_unknown_keys_and_bad_values() {
        assert!(Config::from_toml("[redis]\nurl_env = \"X\"\nurl = \"redis://h\"\n").is_err());
        let long_prefix = format!(
            "[redis]\nurl_env = \"X\"\nkey_prefix = \"{}\"\n",
            "a".repeat(65)
        );
        for (toml, needle) in [
            ("[redis]\nurl_env = \"\"\n".to_string(), "redis.url_env"),
            (
                "[redis]\nurl_env = \"HAS SPACE\"\n".to_string(),
                "redis.url_env",
            ),
            (
                "[redis]\nurl_env = \"X\"\nkey_prefix = \"\"\n".to_string(),
                "redis.key_prefix",
            ),
            (
                "[redis]\nurl_env = \"X\"\nkey_prefix = \"a b\"\n".to_string(),
                "redis.key_prefix",
            ),
            (
                "[redis]\nurl_env = \"X\"\nkey_prefix = \"a{b\"\n".to_string(),
                "redis.key_prefix",
            ),
            (
                "[redis]\nurl_env = \"X\"\nkey_prefix = \"a}\"\n".to_string(),
                "redis.key_prefix",
            ),
            (long_prefix, "redis.key_prefix"),
        ] {
            let message = Config::from_toml(&toml).unwrap().validate().unwrap_err();
            assert!(message.contains(needle), "{toml}: {message}");
        }
    }
    use super::*;

    #[test]
    fn a_mail_providers_name_is_the_spelling_the_config_takes() {
        for provider in [
            MailProvider::Smtp,
            MailProvider::Postmark,
            MailProvider::Ses,
            MailProvider::Log,
        ] {
            // serde reads a bare enum variant from a JSON string.
            let parsed: MailProvider =
                serde_json::from_value(serde_json::Value::from(provider.as_str())).unwrap();

            assert_eq!(parsed, provider);
        }
    }

    #[test]
    fn an_empty_file_gives_the_defaults() {
        let config = Config::from_toml("").unwrap();

        assert_eq!(config, Config::default());
        assert_eq!(config.max_import_bytes.as_u64(), 64 * 1024 * 1024);
        assert_eq!(config.listen.to_string(), "0.0.0.0:8080");
        assert_eq!(config.allowed_hosts, ["localhost", "127.0.0.1", "::1"]);
        assert_eq!(config.default_catalog, None);
        assert_eq!(config.poll_interval, Duration::from_secs(10));
        assert_eq!(
            config.session_idle_timeout,
            Duration::from_secs(2 * 60 * 60)
        );
        assert_eq!(config.concept_outline_chars, 20_000);
        assert_eq!(config.database_max_connections, 16);
        assert_eq!(config.overlay_cache_size, 64);
    }

    #[test]
    fn every_field_reads_from_the_file_and_unknown_keys_are_ignored() {
        let text = "max_import_bytes = \"1MiB\"\n\
                    listen = \"127.0.0.1:9000\"\n\
                    allowed_hosts = [\"okf-mcp.example\"]\n\
                    default_catalog = \"pg\"\n\
                    poll_interval = \"90s\"\n\
                    session_idle_timeout = \"30m\"\n\
                    time_server = \"ntp.example\"\n\
                    concept_outline_chars = 500\n\
                    database_max_connections = 32\n\
                    overlay_cache_size = 8\n\
                    a_later_key = true\n";

        let config = Config::from_toml(text).unwrap();

        assert_eq!(config.max_import_bytes.as_u64(), 1024 * 1024);
        assert_eq!(config.listen.to_string(), "127.0.0.1:9000");
        assert_eq!(config.allowed_hosts, ["okf-mcp.example"]);
        assert_eq!(config.default_catalog.as_deref(), Some("pg"));
        assert_eq!(config.poll_interval, Duration::from_secs(90));
        assert_eq!(config.session_idle_timeout, Duration::from_secs(30 * 60));
        assert_eq!(config.time_server.as_deref(), Some("ntp.example"));
        assert_eq!(config.concept_outline_chars, 500);
        assert_eq!(config.database_max_connections, 32);
        assert_eq!(config.overlay_cache_size, 8);
    }

    #[test]
    fn time_server_is_optional_and_never_blank() {
        let none = Config::from_toml("").unwrap();
        let named = Config::from_toml("time_server = \"time.example\"\n").unwrap();
        let blank = Config::from_toml("time_server = \"  \"\n").unwrap();
        let padded = Config::from_toml("time_server = \" time.example\\t\"\n").unwrap();

        assert_eq!(none.time_server, None);
        assert_eq!(named.time_server.as_deref(), Some("time.example"));
        assert_eq!(named.validate(), Ok(()));
        assert_eq!(
            blank.validate(),
            Err("time_server is blank; name the NTP server or leave it out".to_string())
        );
        assert_eq!(
            padded.validate(),
            Err("time_server has surrounding whitespace; remove it".to_string())
        );
    }

    #[test]
    fn sizes_take_plain_numbers() {
        let config = Config::from_toml("max_import_bytes = 2048\n").unwrap();
        assert_eq!(config.max_import_bytes.as_u64(), 2048);
    }

    #[test]
    fn a_bad_value_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("okf.toml");
        std::fs::write(&path, "poll_interval = \"soon\"\n").unwrap();

        let error = Config::load(Some(&path)).unwrap_err();

        assert!(matches!(&error, OkfError::Config { path: p, .. } if p.ends_with("okf.toml")));
    }

    #[test]
    fn values_that_would_break_the_server_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("okf.toml");
        for (text, needle) in [
            ("poll_interval = \"0s\"\n", "poll_interval"),
            ("session_idle_timeout = \"0s\"\n", "session_idle_timeout"),
            ("allowed_hosts = []\n", "allowed_hosts"),
            ("database_max_connections = 0\n", "database_max_connections"),
            ("overlay_cache_size = 0\n", "overlay_cache_size"),
            (
                "allowed_hosts = [\"\"]\n",
                "allowed_hosts has a blank entry",
            ),
            (
                "allowed_hosts = [\"localhost\", \"  \"]\n",
                "allowed_hosts has a blank entry",
            ),
        ] {
            std::fs::write(&path, text).unwrap();

            let error = Config::load(Some(&path)).unwrap_err();

            assert!(
                matches!(&error, OkfError::Config { path: p, .. } if p.ends_with("okf.toml")),
                "{text}: {error:?}"
            );
            assert!(error.to_string().contains(needle), "{text}: {error}");
        }
    }

    #[test]
    fn maintenance_review_interval_is_per_catalog() {
        let default = Config::default();
        assert_eq!(
            default.maintenance_review_interval("pg").num_seconds(),
            2_592_000
        );
        let config =
            Config::from_toml("[maintenance_review_intervals]\npg = \"14d\"\nother = \"1d\"\n")
                .unwrap();
        assert_eq!(
            config.maintenance_review_interval("pg").num_seconds(),
            1_209_600
        );
        assert_eq!(
            config.maintenance_review_interval("other").num_seconds(),
            86_400
        );
        assert_eq!(
            config.maintenance_review_interval("missing").num_seconds(),
            2_592_000
        );
        assert_eq!(config.validate(), Ok(()));
        assert!(
            Config::from_toml("[maintenance_review_intervals]\npg = \"not-duration\"\n").is_err()
        );
        let too_short =
            Config::from_toml("[maintenance_review_intervals]\npg = \"86399s\"\n").unwrap();
        assert!(too_short.validate().is_err());
        let invalid_name =
            Config::from_toml("[maintenance_review_intervals]\n\"Bad_Name\" = \"1d\"\n").unwrap();
        assert!(invalid_name.validate().is_err());
    }

    #[test]
    fn no_file_gives_the_defaults() {
        assert_eq!(Config::load(None).unwrap(), Config::default());
    }

    /// The defaults with OAuth on at `public_url`, its host allowed.
    fn oauth_on(public_url: &str, allowed: &[&str]) -> Config {
        Config {
            public_url: Some(public_url.to_string()),
            allowed_hosts: allowed.iter().map(|host| host.to_string()).collect(),
            ..Config::default()
        }
    }

    #[test]
    fn oauth_is_off_by_default_with_sensible_lifetimes() {
        let config = Config::default();

        assert_eq!(config.public_url, None);
        assert_eq!(config.oauth.access_token_ttl, Duration::from_secs(60 * 60));
        assert_eq!(
            config.oauth.refresh_token_ttl,
            Duration::from_secs(30 * 24 * 60 * 60)
        );
        assert_eq!(
            config.oauth.idle_grant_revoke_after,
            Duration::from_secs(30 * 24 * 60 * 60)
        );
        assert_eq!(
            config.oauth.code_max_expiry,
            Duration::from_secs(7 * 24 * 60 * 60)
        );
        assert!(config.oauth.dcr_enabled);
        assert!(!config.oauth.cimd_allow_private);
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn the_oauth_section_reads_from_the_file() {
        let text = "public_url = \"https://okf.example.com\"\n\
                    allowed_hosts = [\"okf.example.com\"]\n\
                    [oauth]\n\
                    access_token_ttl = \"15m\"\n\
                    refresh_token_ttl = \"7d\"\n\
                    idle_grant_revoke_after = \"60d\"\n\
                    code_max_expiry = \"2d\"\n\
                    dcr_enabled = false\n\
                    cimd_allow_private = true\n";

        let config = Config::from_toml(text).unwrap();

        assert_eq!(
            config.public_url.as_deref(),
            Some("https://okf.example.com")
        );
        assert_eq!(config.oauth.access_token_ttl, Duration::from_secs(15 * 60));
        assert_eq!(
            config.oauth.refresh_token_ttl,
            Duration::from_secs(7 * 24 * 60 * 60)
        );
        assert_eq!(
            config.oauth.idle_grant_revoke_after,
            Duration::from_secs(60 * 24 * 60 * 60)
        );
        assert_eq!(
            config.oauth.code_max_expiry,
            Duration::from_secs(2 * 24 * 60 * 60)
        );
        assert!(!config.oauth.dcr_enabled);
        assert!(config.oauth.cimd_allow_private);
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn public_url_must_be_an_allowed_https_origin() {
        // Accepted: https on an allowed host, with or without a trailing
        // slash or an explicit port that an entry allows; http on loopback.
        for config in [
            oauth_on("https://okf.example.com", &["okf.example.com"]),
            oauth_on("https://okf.example.com/", &["okf.example.com"]),
            oauth_on("https://OKF.example.com", &["okf.example.com"]),
            oauth_on("https://okf.example.com:8443", &["okf.example.com:8443"]),
            oauth_on("https://okf.example.com:8443", &["okf.example.com"]),
            oauth_on("http://127.0.0.1:8080", &["127.0.0.1"]),
            oauth_on("http://localhost:8080", &["localhost"]),
            oauth_on("http://[::1]:8080", &["::1"]),
        ] {
            assert_eq!(config.validate(), Ok(()), "{:?}", config.public_url);
        }
        // Refused, each with a reason that names the problem.
        for (config, needle) in [
            (
                oauth_on("http://okf.example.com", &["okf.example.com"]),
                "https",
            ),
            (
                oauth_on("ftp://okf.example.com", &["okf.example.com"]),
                "https",
            ),
            (
                oauth_on("https://okf.example.com/okf", &["okf.example.com"]),
                "origin",
            ),
            (
                oauth_on("https://okf.example.com/?a=b", &["okf.example.com"]),
                "origin",
            ),
            (
                oauth_on("https://user@okf.example.com", &["okf.example.com"]),
                "user info",
            ),
            (oauth_on("okf.example.com", &["okf.example.com"]), "scheme"),
            (
                oauth_on("https://other.example.com", &["okf.example.com"]),
                "allowed_hosts",
            ),
            (
                oauth_on("https://okf.example.com", &["okf.example.com:8443"]),
                "allowed_hosts",
            ),
            (oauth_on("not a url", &["okf.example.com"]), "URL"),
        ] {
            let error = config.validate().expect_err("refused");
            assert!(error.contains(needle), "{:?}: {error}", config.public_url);
        }
    }

    #[test]
    fn oauth_lifetimes_must_be_positive() {
        // Each closure is a plain `fn` pointer (it captures nothing), so the
        // four share one type and fit in one array.
        let changes: [fn(&mut OAuthConfig); 4] = [
            |oauth| oauth.access_token_ttl = Duration::ZERO,
            |oauth| oauth.refresh_token_ttl = Duration::ZERO,
            |oauth| oauth.idle_grant_revoke_after = Duration::ZERO,
            |oauth| oauth.code_max_expiry = Duration::ZERO,
        ];
        for change in changes {
            let mut config = Config::default();
            change(&mut config.oauth);

            let error = config.validate().expect_err("refused");

            assert!(error.contains("more than zero"), "{error}");
        }
    }

    #[test]
    fn auth_defaults_match_the_spec() {
        let auth = Config::from_toml("").unwrap().auth;

        assert_eq!(auth, AuthConfig::default());
        assert!(auth.password_sign_in);
        assert_eq!(auth.max_keys_per_user, 20);
        assert_eq!(auth.max_key_lifetime, None);
        assert_eq!(auth.console_session_idle, Duration::from_secs(12 * 60 * 60));
        assert_eq!(
            auth.console_session_absolute,
            Duration::from_secs(7 * 24 * 60 * 60)
        );
        assert_eq!(auth.setup_link_ttl, Duration::from_secs(24 * 60 * 60));
        assert_eq!(auth.reset_link_ttl, Duration::from_secs(60 * 60));
        assert_eq!(auth.trusted_proxy_header, None);
    }

    #[test]
    fn every_auth_key_reads_from_the_file() {
        let text = "[auth]\n\
                    password_sign_in = false\n\
                    max_keys_per_user = 5\n\
                    max_key_lifetime = \"90d\"\n\
                    console_session_idle = \"30m\"\n\
                    console_session_absolute = \"1d\"\n\
                    setup_link_ttl = \"2d\"\n\
                    reset_link_ttl = \"15m\"\n\
                    trusted_proxy_header = \"x-forwarded-for\"\n";

        let config = Config::from_toml(text).unwrap();
        let auth = &config.auth;

        assert_eq!(config.validate(), Ok(()));
        assert!(!auth.password_sign_in);
        assert_eq!(auth.max_keys_per_user, 5);
        assert_eq!(
            auth.max_key_lifetime,
            Some(Duration::from_secs(90 * 24 * 60 * 60))
        );
        assert_eq!(auth.console_session_idle, Duration::from_secs(30 * 60));
        assert_eq!(
            auth.console_session_absolute,
            Duration::from_secs(24 * 60 * 60)
        );
        assert_eq!(auth.setup_link_ttl, Duration::from_secs(2 * 24 * 60 * 60));
        assert_eq!(auth.reset_link_ttl, Duration::from_secs(15 * 60));
        assert_eq!(
            auth.trusted_proxy_header.as_deref(),
            Some("x-forwarded-for")
        );
    }

    #[test]
    fn auth_values_that_would_break_sign_in_are_refused() {
        let refused = [
            "[auth]\nconsole_session_idle = \"0s\"\n",
            "[auth]\nconsole_session_absolute = \"0s\"\n",
            "[auth]\nsetup_link_ttl = \"0s\"\n",
            "[auth]\nreset_link_ttl = \"0s\"\n",
            "[auth]\nmax_key_lifetime = \"0s\"\n",
            "[auth]\nreset_link_ttl = \"300000000years\"\n",
            "[auth]\nconsole_session_absolute = \"1000000years\"\n",
            "[auth]\nsetup_link_ttl = \"10years 1s\"\n",
            "[oauth]\nrefresh_token_ttl = \"1000000years\"\n",
            "[auth]\nmax_keys_per_user = 1001\n",
            "[auth]\ntrusted_proxy_header = \"not a header\"\n",
            "[auth]\ntrusted_proxy_header = \"\"\n",
        ];

        for text in refused {
            let config = Config::from_toml(text).unwrap();
            assert!(config.validate().is_err(), "{text}");
        }
        let at_the_limit = Config::from_toml("[auth]\nmax_keys_per_user = 1000\n").unwrap();
        assert_eq!(at_the_limit.validate(), Ok(()));
        let longest =
            Config::from_toml("[auth]\nconsole_session_absolute = \"10years\"\n").unwrap();
        assert_eq!(longest.validate(), Ok(()));
        let too_long = Config::from_toml("[auth]\nconsole_session_absolute = \"1000000years\"\n")
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(
            too_long.contains("auth.console_session_absolute") && too_long.contains("10 years"),
            "{too_long}"
        );
    }

    #[test]
    fn mail_defaults_to_the_log_provider_with_a_worker() {
        let mail = Config::from_toml("").unwrap().mail;

        assert_eq!(mail, MailConfig::default());
        assert_eq!(mail.provider, MailProvider::Log);
        assert_eq!(mail.from, "okf <okf@localhost>");
        assert_eq!(mail.worker, None);
        assert!(Config::default().deprecation_warnings().is_empty());
        assert!(Config::default().validate().is_ok());
    }

    #[test]
    fn mail_worker_false_parses_with_a_warning_and_is_ignored() {
        let config = Config::from_toml("[mail]\nworker = false\n").unwrap();

        assert_eq!(config.mail.worker, Some(false));
        assert!(
            config.validate().is_ok(),
            "an old configuration still starts"
        );
        assert_eq!(
            config.deprecation_warnings(),
            ["mail.worker is deprecated and ignored: every serve runs the worker"]
        );
    }

    #[test]
    fn worker_shutdown_grace_defaults_reads_and_must_be_positive() {
        assert_eq!(
            Config::default().worker_shutdown_grace,
            Duration::from_secs(30)
        );
        let config = Config::from_toml("worker_shutdown_grace = \"45s\"\n").unwrap();
        assert_eq!(config.worker_shutdown_grace, Duration::from_secs(45));
        let error = Config::from_toml("worker_shutdown_grace = \"0s\"\n")
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(error.contains("worker_shutdown_grace"), "{error}");
    }

    #[test]
    fn every_mail_key_reads_from_the_file() {
        let text = "[mail]\n\
                    provider = \"smtp\"\n\
                    from = \"okf <okf@example.com>\"\n\
                    worker = false\n\
                    [mail.smtp]\n\
                    host = \"smtp.example.com\"\n\
                    port = 465\n\
                    starttls = false\n\
                    username = \"okf\"\n\
                    password_env = \"OKF_SMTP_PASSWORD\"\n\
                    [mail.postmark]\n\
                    token_env = \"OKF_POSTMARK_TOKEN\"\n\
                    [mail.ses]\n\
                    region = \"us-east-1\"\n";

        let config = Config::from_toml(text).unwrap();

        assert_eq!(config.mail.provider, MailProvider::Smtp);
        assert_eq!(config.mail.from, "okf <okf@example.com>");
        assert_eq!(config.mail.worker, Some(false));
        assert_eq!(
            config.mail.smtp,
            Some(SmtpConfig {
                host: "smtp.example.com".to_string(),
                port: 465,
                starttls: false,
                username: Some("okf".to_string()),
                password_env: Some("OKF_SMTP_PASSWORD".to_string()),
            })
        );
        assert_eq!(
            config.mail.postmark,
            Some(PostmarkConfig {
                token_env: "OKF_POSTMARK_TOKEN".to_string(),
                message_stream: "outbound".to_string(),
            })
        );
        assert_eq!(
            config.mail.ses,
            Some(SesConfig {
                region: Some("us-east-1".to_string()),
            })
        );
        assert!(config.validate().is_ok());
    }

    #[test]
    fn ses_needs_no_table_and_takes_an_optional_region() {
        for text in [
            "[mail]\nprovider = \"ses\"\n",
            "[mail]\nprovider = \"ses\"\n[mail.ses]\n",
            "[mail]\nprovider = \"ses\"\n[mail.ses]\nregion = \"eu-west-1\"\n",
        ] {
            let config = Config::from_toml(text).unwrap();

            assert_eq!(config.mail.provider, MailProvider::Ses, "{text}");
            assert!(config.validate().is_ok(), "{text}");
        }
    }

    #[test]
    fn smtp_defaults_to_starttls_on_587() {
        let text = "[mail]\nprovider = \"smtp\"\n[mail.smtp]\nhost = \"smtp.example.com\"\n";

        let smtp = Config::from_toml(text).unwrap().mail.smtp.unwrap();

        assert_eq!((smtp.port, smtp.starttls), (587, true));
    }

    #[test]
    fn mail_settings_that_cannot_send_are_refused() {
        let smtp = "[mail]\nprovider = \"smtp\"\n[mail.smtp]\nhost = \"smtp.example.com\"\n";
        let refused = [
            "[mail]\nprovider = \"smtp\"\n".to_string(),
            "[mail]\nprovider = \"postmark\"\n".to_string(),
            "[mail]\nfrom = \"not an address\"\n".to_string(),
            "[mail]\nprovider = \"smtp\"\n[mail.smtp]\nhost = \" \"\n".to_string(),
            format!("{smtp}username = \"okf\"\n"),
            format!("{smtp}password_env = \"OKF_SMTP_PASSWORD\"\n"),
            format!("{smtp}username = \"okf\"\npassword_env = \" \"\n"),
            format!("{smtp}port = 0\n"),
            "[mail]\nprovider = \"postmark\"\n[mail.postmark]\ntoken_env = \"\"\n".to_string(),
            "[mail]\nprovider = \"postmark\"\n[mail.postmark]\ntoken_env = \"T\"\n\
             message_stream = \" \"\n"
                .to_string(),
            "[mail]\nprovider = \"ses\"\n[mail.ses]\nregion = \" \"\n".to_string(),
        ];

        for text in &refused {
            let config = Config::from_toml(text).unwrap();
            assert!(config.validate().is_err(), "{text}");
        }
        assert!(Config::from_toml("[mail]\nprovider = \"pigeon\"\n").is_err());
    }

    const OIDC: &str = r#"
public_url = "https://okf.example.com"
allowed_hosts = ["okf.example.com"]

[[auth.oidc]]
id = "example"
label = "Example ID"
issuer = "https://id.example.com"
client_id = "okf-console"
client_secret_env = "OKF_OIDC_EXAMPLE_SECRET"
"#;

    #[test]
    fn oidc_providers_parse_from_an_array_of_tables_with_default_scopes() {
        let config = Config::from_toml(OIDC).expect("parses");

        config.validate().expect("valid");
        assert_eq!(config.auth.oidc.len(), 1);
        assert_eq!(config.auth.oidc[0].id, "example");
        assert_eq!(config.auth.oidc[0].scopes, ["openid", "email", "profile"]);
    }

    #[test]
    fn bad_oidc_providers_are_refused() {
        let base = Config::from_toml(OIDC).expect("parses");
        // Each case changes one thing and names a word the refusal must
        // contain. `Change` is a plain function pointer; closures that
        // capture nothing coerce to one.
        type Change = fn(&mut Config);
        let cases: [(&str, Change); 8] = [
            ("public_url", |config| config.public_url = None),
            ("id", |config| {
                config.auth.oidc[0].id = "Example".to_string()
            }),
            ("twice", |config| {
                let again = config.auth.oidc[0].clone();
                config.auth.oidc.push(again);
            }),
            ("label", |config| {
                config.auth.oidc[0].label = " ".to_string()
            }),
            ("client_id", |config| {
                config.auth.oidc[0].client_id = String::new()
            }),
            ("client_secret_env", |config| {
                config.auth.oidc[0].client_secret_env = String::new()
            }),
            ("issuer", |config| {
                config.auth.oidc[0].issuer = "http://id.example.com".to_string()
            }),
            ("issuer", |config| {
                config.auth.oidc[0].issuer = "https://id.example.com/?tenant=1".to_string()
            }),
        ];
        for (word, change) in cases {
            let mut config = base.clone();
            change(&mut config);
            let refused = config.validate().expect_err(word);
            assert!(refused.contains(word), "{word}: {refused}");
        }
    }

    #[test]
    fn an_oidc_issuer_may_be_http_on_loopback_only() {
        let mut config = Config::from_toml(OIDC).expect("parses");
        config.auth.oidc[0].issuer = "http://127.0.0.1:9000".to_string();

        config
            .validate()
            .expect("loopback http is allowed, for tests");
    }

    #[test]
    fn an_unknown_oidc_key_is_refused_at_parse_time() {
        let text = format!("{OIDC}client_secret = \"inline\"\n");

        assert!(
            Config::from_toml(&text).is_err(),
            "secrets come from the environment only"
        );
    }
}

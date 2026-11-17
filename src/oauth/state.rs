//! What the authorization server keeps between requests (spec "In
//! memory"): pending authorizations (a consent page in progress) and
//! authorization codes (minted on Approve, exchanged once at `/oauth/token`).
//! With `[redis]` both are kept in Redis, so every replica sees them.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::store::ceiling::Ceiling;
use crate::store::oauth::clients::ClientRecord;

/// How long a consent page may stay open.
pub const PENDING_TTL: Duration = Duration::from_secs(10 * 60);

/// How long an authorization code may wait to be exchanged.
pub const AUTHORIZATION_CODE_TTL: Duration = Duration::from_secs(60);

/// Wrong sign-in codes a pending authorization survives.
pub const MAX_FAILED_ATTEMPTS: u8 = 5;

/// The most pending authorizations held at once in memory (they are made by
/// an unauthenticated GET, so the map is bounded). Redis drops the global cap
/// (multi-replica spec Decision 8).
pub const MAX_PENDING: usize = 10_000;

/// The most unexchanged authorization codes held at once.
pub const MAX_AUTHORIZATION_CODES: usize = 10_000;

/// A validated `GET /oauth/authorize`, waiting for the person's decision.
/// Keyed in `OAuth::pending` by a random id the consent form carries. With
/// `[redis]` it is stored there as JSON under the id's hash (multi-replica
/// spec Decision 7): it holds the CSRF token and nonces, so Redis is trusted
/// as Postgres is.
// `Serialize, Deserialize` (serde) write and read the JSON; `Debug` stays
// hand-written below.
#[derive(Clone, Serialize, Deserialize)]
pub struct PendingAuthorization {
    /// The CSRF token every consent form must echo.
    pub csrf: String,
    /// The client, resolved (CIMD or DCR).
    pub client: ClientRecord,
    /// The redirect URI, already matched exactly against the client's.
    pub redirect_uri: String,
    /// The client's `state`, returned unchanged.
    pub state: Option<String>,
    /// The PKCE S256 challenge.
    pub code_challenge: String,
    /// The resource (always this server's `/mcp`).
    pub resource: String,
    /// The sign-in code accepted at step 1 and the step-2 page showing it,
    /// awaiting Approve.
    pub accepted: Option<AcceptedCode>,
    /// Wrong codes entered so far.
    pub failed_attempts: u8,
    /// Set when a signed-in person's consent page was shown (the user
    /// path); `None` on the sign-in code path.
    pub consent: Option<UserConsent>,
    /// The request's validated parameters as a query string (no `?`, no
    /// `signin`), for the sign-in code link when the page is shown again.
    pub query: String,
}

// Written by hand, not derived, so a stray `{:?}` never prints the CSRF token.
impl std::fmt::Debug for PendingAuthorization {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "Display/Debug impl: runs while the subscriber formats an event"
        )
    )]
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingAuthorization")
            .field("csrf", &"<redacted>")
            .field("client_id", &self.client.client_id)
            .field("redirect_uri", &self.redirect_uri)
            .field("accepted", &self.accepted)
            .field("failed_attempts", &self.failed_attempts)
            .field("consent", &self.consent)
            .finish_non_exhaustive()
    }
}

/// A sign-in code accepted at step 1, bound to the one step-2 summary that
/// shows it. Entering another code (in another copy of the step-1 form)
/// replaces both, so an older summary's Approve no longer matches and
/// cannot approve a code that page did not show.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedCode {
    /// The code's id (never the code).
    pub code_id: String,
    /// A random token in that summary's Approve form, compared in constant
    /// time on Approve.
    pub summary_nonce: String,
}

// Written by hand, not derived, so a stray `{:?}` never prints the nonce.
impl std::fmt::Debug for AcceptedCode {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "Display/Debug impl: runs while the subscriber formats an event"
        )
    )]
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcceptedCode")
            .field("code_id", &self.code_id)
            .field("summary_nonce", &"<redacted>")
            .finish()
    }
}

/// A signed-in person's consent page: who it was shown to, what it
/// offered, and the random token its Approve must echo. Each Approve spends
/// the token (it is replaced in one atomic update of the pending
/// authorization, a compare-and-set in Redis), so one page approves at most
/// once, across every replica.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserConsent {
    /// The `users.id` the page was rendered for; the POST's session must
    /// still be this user.
    pub user_id: String,
    /// A random token in the page's form (its hidden `summary` input),
    /// compared in constant time.
    pub nonce: String,
    /// The ceiling the page offered. The POST may choose only from it, and
    /// the store checks the choice against the ceiling again at insert.
    pub offered: Ceiling,
}

// Written by hand, not derived, so a stray `{:?}` never prints the nonce.
impl std::fmt::Debug for UserConsent {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "Display/Debug impl: runs while the subscriber formats an event"
        )
    )]
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UserConsent")
            .field("user_id", &self.user_id)
            .field("nonce", &"<redacted>")
            .field("offered", &self.offered)
            .finish()
    }
}

/// An authorization code's binding, keyed in `OAuth::codes` by the hex
/// SHA-256 of the `okfac_` code (the code itself is never stored). With
/// `[redis]` it is stored there as JSON, so every replica can exchange it
/// (spec Decision 5); it holds no secret, only bindings and flags.
// `Serialize, Deserialize` (serde) write the JSON encoding and decoding,
// like a Python dataclass with `asdict` and a matching constructor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizationCode {
    /// The grant the approval created.
    pub grant_id: String,
    /// The client it was issued to.
    pub client_id: String,
    /// The redirect URI it was issued with.
    pub redirect_uri: String,
    /// The PKCE S256 challenge the verifier must match.
    pub code_challenge: String,
    /// The resource the tokens will be issued for.
    pub resource: String,
    /// The token family its tokens join.
    pub family_id: String,
    /// Whether it was exchanged. A used code stays in the store until it
    /// expires, so presenting it again is seen as a replay (RFC 6749
    /// section 4.1.2: Task 6 then revokes its family).
    pub used: bool,
    /// Whether it was presented again after it was used. The exchange that
    /// used it checks this after its tokens commit, so a replay racing that
    /// exchange still revokes the tokens it mints (Task 6).
    pub replayed: bool,
}

/// The key of `OAuth::codes` for an authorization code: the hex SHA-256 of
/// the `okfac_` code, so the map never holds a usable code.
///
/// # Arguments
/// * `code` - the authorization code, as minted or presented.
///
/// # Returns
/// 64 lowercase hex characters.
#[tracing::instrument(level = "debug", skip_all)]
pub fn code_key(code: &str) -> String {
    crate::shared::hashed(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::oauth::clients::ClientKind;

    #[test]
    fn code_key_is_the_hex_sha256() {
        // The FIPS 180-2 test vector for "abc" (as in `keys::tests`).
        assert_eq!(
            code_key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn debug_never_prints_the_consent_nonce() {
        let consent = UserConsent {
            user_id: "user-id".into(),
            nonce: "consent-nonce-that-must-not-leak".into(),
            offered: Ceiling::new(),
        };

        let printed = format!("{consent:?}");

        assert!(
            !printed.contains("consent-nonce-that-must-not-leak"),
            "{printed}"
        );
        assert!(printed.contains("user-id"), "{printed}");
    }

    /// A pending authorization with every field set.
    fn sample_pending() -> PendingAuthorization {
        PendingAuthorization {
            csrf: "csrf-value-that-must-not-leak".into(),
            client: ClientRecord {
                client_id: "https://client.example.com/client.json".into(),
                kind: ClientKind::Cimd,
                client_name: None,
                redirect_uris: vec!["https://client.example.com/callback".into()],
                metadata: serde_json::json!({}),
                fetched_at: None,
                cache_expires_at: None,
                created_at: chrono::Utc::now(),
            },
            redirect_uri: "https://client.example.com/callback".into(),
            state: None,
            code_challenge: "c".repeat(43),
            resource: "https://okf.example.com/mcp".into(),
            accepted: Some(AcceptedCode {
                code_id: "code-id".into(),
                summary_nonce: "nonce-value-that-must-not-leak".into(),
            }),
            failed_attempts: 0,
            consent: Some(UserConsent {
                user_id: "user-id".into(),
                nonce: "consent-nonce-that-must-not-leak".into(),
                offered: Ceiling::new(),
            }),
            query: "state=state-that-stays-out".into(),
        }
    }

    /// What Redis stores (M16 Task 8) reads back as the same value.
    #[test]
    fn a_pending_authorization_round_trips_through_json() {
        let mut pending = sample_pending();
        pending.state = Some("xyz".into());
        pending.failed_attempts = 3;
        pending.consent = Some(UserConsent {
            user_id: "user-id".into(),
            nonce: "nonce".into(),
            offered: [("main".to_string(), crate::store::keys::Access::Write)]
                .into_iter()
                .collect(),
        });

        let json = serde_json::to_value(&pending).expect("serializes");
        let back: PendingAuthorization = serde_json::from_value(json.clone()).expect("reads back");

        assert_eq!(json["consent"]["offered"]["main"], "write");
        assert_eq!(json["client"]["kind"], "cimd");
        assert_eq!(serde_json::to_value(&back).expect("serializes"), json);
        assert_eq!(back.accepted, pending.accepted);
        assert_eq!(back.consent, pending.consent);
        assert_eq!(back.client, pending.client);
    }

    #[test]
    fn debug_never_prints_the_csrf_token() {
        let pending = sample_pending();

        let printed = format!("{pending:?}");

        assert!(!printed.contains("csrf-value-that-must-not-leak"));
        assert!(!printed.contains("nonce-value-that-must-not-leak"));
        assert!(!printed.contains("consent-nonce-that-must-not-leak"));
        assert!(!printed.contains("state-that-stays-out"));
        assert!(printed.contains("user-id"));
        assert!(printed.contains("code-id"));
        assert!(printed.contains("<redacted>"));
    }
}

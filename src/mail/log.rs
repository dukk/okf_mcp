//! The `log` provider: writes each message to the log at `info`, link
//! included. For development and tests: it delivers nothing. It is the one
//! place a message body reaches the log, by design (`from_config` says so
//! when the server starts).
//!
//! The event goes to `LOCAL_ONLY`: the message holds the reset or sign-in
//! link, a usable credential, so it reaches stderr and never an OTLP exporter.

use super::{Mail, MailError, Mailer};
use crate::telemetry::subscriber::LOCAL_ONLY;

/// Logs instead of sending.
pub(crate) struct LogMailer;

#[async_trait::async_trait]
impl Mailer for LogMailer {
    #[tracing::instrument(level = "debug", skip_all)]
    async fn send(&self, mail: &Mail) -> Result<(), MailError> {
        // `%value` records a field with its `Display` form.
        tracing::info!(
            target: LOCAL_ONLY,
            to = %mail.to,
            subject = %mail.subject,
            text = %mail.text,
            "mail (log provider; not delivered)"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_reaches_stderr_and_neither_otlp_layer() {
        use crate::telemetry::config::LogFormat;
        use crate::telemetry::subscriber::capture::observe;

        let mail = Mail {
            to: "alice@example.com".to_string(),
            subject: "reset".to_string(),
            text: "https://okf.example.com/reset?token=SENTINEL-RESET-TOKEN".to_string(),
            html: String::new(),
        };
        let observed = observe("trace", "trace", "trace", LogFormat::Text, || {
            // The mailer is async; a one-thread runtime keeps the event on
            // this thread, under the scoped subscriber.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime");
            let span = tracing::info_span!(target: "okf_mcp::test", "send");
            let _entered = span.enter();
            runtime.block_on(LogMailer.send(&mail)).expect("sends");
        });
        assert!(
            observed.stderr.contains("SENTINEL-RESET-TOKEN"),
            "{}",
            observed.stderr
        );
        assert!(
            observed
                .logs
                .iter()
                .all(|log| !log.contains("SENTINEL-RESET-TOKEN")),
            "{:?}",
            observed.logs
        );
        let span = observed
            .spans
            .iter()
            .find(|span| span.name == "send")
            .expect("span");
        assert!(!format!("{span:?}").contains("SENTINEL-RESET-TOKEN"));
        assert!(span.events.is_empty(), "{:?}", span.events);
    }

    #[tokio::test]
    async fn the_log_provider_always_succeeds() {
        let mail = Mail {
            to: "alice@example.com".to_string(),
            subject: "okf test email".to_string(),
            text: "hello".to_string(),
            html: "<p>hello</p>".to_string(),
        };

        assert_eq!(LogMailer.send(&mail).await, Ok(()));
    }
}

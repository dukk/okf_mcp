//! The `okf-mcp` binary: parses the command line and runs it
//! ([`okf_mcp::cli::run`]).

use std::sync::Arc;

use clap::Parser;
use okf_mcp::cli::Cli;
use okf_mcp::clock::SystemClock;

/// Runs the command line with the system clock. `okf_mcp::cli::run` installs
/// the `tracing` subscriber (stderr, and OTLP for `serve`), after it has read
/// the configuration file.
// `#[tokio::main]` wraps `main` in a tokio runtime, so it can `.await`.
// Returning `anyhow::Result` prints the error chain and exits non-zero.
#[tokio::main]
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "runs before the subscriber exists: it only hands the parsed command line to cli::run, which installs the subscriber"
    )
)]
async fn main() -> anyhow::Result<()> {
    // `Arc::new` puts the clock on the heap behind a shared handle; the server
    // keeps one for as long as it runs.
    okf_mcp::cli::run(Cli::parse(), Arc::new(SystemClock)).await
}

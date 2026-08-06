//! stdio transport.
//!
//! Shutdown here is "abort the serving task", not a graceful drain: `serve()`
//! reads stdin on a blocking OS thread, which the same signal can interrupt
//! (EINTR), nondeterministically finishing that future with a spurious
//! transport error instead of letting the signal branch win. `main` therefore
//! races a *spawned* task against the signal and aborts it — see ADR 0005.

use anyhow::Context as _;
use rmcp::ServiceExt as _;

use crate::server::RedmineMcp;

/// Serve MCP over stdin/stdout until the client disconnects.
///
/// # Errors
///
/// Fails if the transport cannot be established or the serving task panics.
pub async fn serve(server: RedmineMcp) -> anyhow::Result<()> {
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .context("failed to start the stdio MCP transport")?;
    service.waiting().await.context("MCP server task failed")?;
    Ok(())
}

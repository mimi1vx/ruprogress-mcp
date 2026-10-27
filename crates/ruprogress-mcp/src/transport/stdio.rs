//! stdio transport.
//!
//! Shutdown here is "abort the serving task", not a graceful drain: `serve()`
//! reads stdin on a blocking OS thread, which the same signal can interrupt
//! (EINTR), nondeterministically finishing that future with a spurious
//! transport error instead of letting the signal branch win. `main` therefore
//! races a *spawned* task against the signal and aborts it — see ADR 0005.

use anyhow::Context as _;
use rmcp::RoleServer;
use rmcp::ServiceExt as _;
use rmcp::transport::async_rw::AsyncRwTransport;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::server::RedmineMcp;
use crate::tools::issues::ISSUE_UPLOADS_MAX_TOTAL_BYTES;

/// Largest single JSON-RPC line accepted on stdio. rmcp drops longer lines
/// without replying, so this must stay above the largest base64 upload request.
pub const MAX_LINE_BYTES: usize = 144 * 1024 * 1024;

const _: () = assert!(
    MAX_LINE_BYTES as u64 > ISSUE_UPLOADS_MAX_TOTAL_BYTES.div_ceil(3) * 4 + 1024 * 1024,
    "MAX_LINE_BYTES must exceed the uploads[] aggregate budget as base64 plus JSON overhead"
);

/// Build the stdio transport over `read`/`write` with [`MAX_LINE_BYTES`].
pub fn transport<R, W>(read: R, write: W) -> AsyncRwTransport<RoleServer, R, W>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    AsyncRwTransport::new_server(read, write).with_max_line_length(MAX_LINE_BYTES)
}

/// Serve MCP over stdin/stdout until the client disconnects.
///
/// # Errors
///
/// Fails if the transport cannot be established or the serving task panics.
pub async fn serve(server: RedmineMcp) -> anyhow::Result<()> {
    let service = server
        .serve(transport(tokio::io::stdin(), tokio::io::stdout()))
        .await
        .context("failed to start the stdio MCP transport")?;
    service.waiting().await.context("MCP server task failed")?;
    Ok(())
}

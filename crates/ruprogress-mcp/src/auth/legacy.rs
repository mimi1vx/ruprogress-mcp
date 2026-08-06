//! `AuthMode::Legacy`: a single credential configured at startup, applied to
//! every request via `RedmineClient::as_default`.

use redmine_client::{RedmineClient, Scoped};
use rmcp::ErrorData as McpError;

use crate::error::to_mcp_error;

pub(crate) fn scoped(client: &RedmineClient) -> Result<Scoped<'_>, McpError> {
    client.as_default().map_err(to_mcp_error)
}

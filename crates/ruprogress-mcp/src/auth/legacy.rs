//! `AuthMode::Legacy`: a single credential configured at startup, applied to
//! every request via `RedmineClient::as_default`.

use redmine_client::{RedmineClient, Scoped};
use rmcp::ErrorData as McpError;

/// `client.as_default()` only fails when the server itself was wired up
/// without a default credential — a static misconfiguration caught by
/// `Config::from_map` in every code path that reaches here, not a per-call
/// Redmine API error. Stays a protocol error for the same reason the
/// `LegacyPerUser`/`OAuth` "not implemented" arms next to this call site in
/// `server.rs::scoped` do: it happens before any tool's Redmine request is
/// even attempted, so `error::to_tool_error`'s Redmine-error envelope (D4)
/// does not apply.
pub(crate) fn scoped(client: &RedmineClient) -> Result<Scoped<'_>, McpError> {
    client
        .as_default()
        .map_err(|e| McpError::internal_error(e.to_string(), None))
}

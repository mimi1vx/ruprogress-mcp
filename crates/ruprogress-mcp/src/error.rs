//! Maps `redmine_client::Error` to `McpError` with an actionable message.
//! Shared by every tool so error text is consistent and never leaks a URL or
//! credential (`redmine_client::Error` already guarantees the latter).

use rmcp::ErrorData as McpError;

pub(crate) fn to_mcp_error(e: redmine_client::Error) -> McpError {
    use redmine_client::Error;
    match e {
        Error::Unauthorized => McpError::internal_error(
            "Redmine rejected the configured credential (401 unauthorized); check REDMINE_API_KEY",
            None,
        ),
        Error::Forbidden => McpError::internal_error(
            "the Redmine account behind the configured credential lacks permission for this operation (403 forbidden)",
            None,
        ),
        Error::NotFound => {
            McpError::resource_not_found("the requested Redmine resource was not found", None)
        }
        Error::RateLimited { .. } => {
            McpError::internal_error("Redmine rate-limited this request; retry later", None)
        }
        Error::Api { status, errors } => McpError::internal_error(
            format!("Redmine returned {status}: {}", errors.join("; ")),
            None,
        ),
        Error::Transport(_) => McpError::internal_error("could not reach the Redmine server", None),
        Error::Decode { context, .. } => McpError::internal_error(
            format!("received an unexpected response shape from Redmine while reading {context}"),
            None,
        ),
        Error::Config { reason } => McpError::internal_error(
            format!("invalid Redmine client configuration: {reason}"),
            None,
        ),
        Error::LimitExceeded {
            what,
            limit,
            actual,
        } => McpError::internal_error(
            format!(
                "Redmine response exceeded the configured limit for {what}: {actual} > {limit}"
            ),
            None,
        ),
        other => McpError::internal_error(format!("Redmine request failed: {other}"), None),
    }
}

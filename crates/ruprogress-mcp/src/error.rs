//! Maps `redmine_client::Error` to the in-band tool-error envelope:
//! `{error, code, retryable, hint}`, returned as a normal `CallToolResult`
//! with `isError: true` so the model can see and react to it, rather than as
//! a protocol-level `McpError`. Shared by every tool so error text is
//! consistent and never leaks a URL or credential (`redmine_client::Error`
//! already guarantees the latter).
//!
//! `McpError` is reserved for argument-validation failures the model must
//! fix before the call is even meaningful (e.g. a malformed project
//! identifier) — those still go through `rmcp`'s own error path, not this
//! module.

use rmcp::model::CallToolResult;

use crate::tools::output::{ErrorCode, err};

pub(crate) fn to_tool_error(e: redmine_client::Error) -> CallToolResult {
    use redmine_client::Error;
    match e {
        Error::Unauthorized => err(
            ErrorCode::Unauthorized,
            "Redmine rejected the configured credential (401 unauthorized)",
            Some(
                "ask the operator to check the configured Redmine credential; do not retry with the same request",
            ),
        ),
        Error::Forbidden => err(
            ErrorCode::Forbidden,
            "the configured Redmine credential lacks permission for this operation (403 forbidden)",
            Some(
                "try a different tool, or ask the user for an account with the required permission",
            ),
        ),
        Error::NotFound => err(
            ErrorCode::NotFound,
            "the requested Redmine resource was not found",
            Some(
                "verify the id/identifier is correct; it may have been deleted, or the credential cannot see it",
            ),
        ),
        Error::RateLimited { .. } => err(
            ErrorCode::RateLimited,
            "Redmine rate-limited this request",
            Some("wait before retrying the same request"),
        ),
        Error::Api { status, errors } if status == http::StatusCode::UNPROCESSABLE_ENTITY => err(
            ErrorCode::ValidationFailed,
            format!("Redmine rejected the request: {}", errors.join("; ")),
            Some("fix the fields named in the error and retry"),
        ),
        Error::Api { status, errors } => err(
            ErrorCode::UnexpectedResponse,
            format!("Redmine returned {status}: {}", errors.join("; ")),
            None,
        ),
        Error::Transport(_) => err(
            ErrorCode::Unreachable,
            "could not reach the Redmine server",
            Some("retry later; do not change the request"),
        ),
        Error::Decode { context, .. } => err(
            ErrorCode::UnexpectedResponse,
            format!("received an unexpected response shape from Redmine while reading {context}"),
            None,
        ),
        Error::Config { reason } => err(
            ErrorCode::Misconfigured,
            format!("invalid Redmine client configuration: {reason}"),
            Some(
                "this is a server configuration problem the model cannot fix; report it to the operator",
            ),
        ),
        Error::LimitExceeded {
            what,
            limit,
            actual,
        } => err(
            ErrorCode::LimitExceeded,
            format!(
                "Redmine response exceeded the configured limit for {what}: {actual} > {limit}"
            ),
            Some("narrow the request (a smaller limit or a more specific filter) and retry"),
        ),
        other => err(
            ErrorCode::UnexpectedResponse,
            format!("Redmine request failed: {other}"),
            None,
        ),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use std::time::Duration;

    use redmine_client::Error;

    use super::*;

    fn code_of(result: &CallToolResult) -> String {
        result.structured_content.as_ref().unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn retryable_of(result: &CallToolResult) -> bool {
        result.structured_content.as_ref().unwrap()["retryable"]
            .as_bool()
            .unwrap()
    }

    /// A real `reqwest::Error` with its URL already stripped, matching what
    /// `redmine_client::Error::transport` always stores — built by forcing a
    /// genuine transport failure against a mock server rather than
    /// hand-constructing one (reqwest gives no public constructor).
    async fn sample_transport_error() -> reqwest::Error {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/secret-path"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        reqwest::Client::new()
            .get(format!("{}/secret-path?key=deadbeef", server.uri()))
            .send()
            .await
            .expect("request should reach the mock server")
            .json::<serde_json::Value>()
            .await
            .expect_err("body is not valid JSON")
            .without_url()
    }

    #[tokio::test]
    async fn every_variant_maps_to_an_in_band_error_with_the_expected_code_and_retryability() {
        let cases: Vec<(Error, &str, bool)> = vec![
            (Error::Unauthorized, "UNAUTHORIZED", false),
            (Error::Forbidden, "FORBIDDEN", false),
            (Error::NotFound, "NOT_FOUND", false),
            (
                Error::RateLimited {
                    retry_after: Some(Duration::from_secs(1)),
                },
                "RATE_LIMITED",
                true,
            ),
            (
                Error::Api {
                    status: http::StatusCode::UNPROCESSABLE_ENTITY,
                    errors: vec!["Subject can't be blank".to_string()],
                },
                "VALIDATION_FAILED",
                false,
            ),
            (
                Error::Api {
                    status: http::StatusCode::INTERNAL_SERVER_ERROR,
                    errors: vec![],
                },
                "UNEXPECTED_RESPONSE",
                false,
            ),
            (
                Error::Transport(sample_transport_error().await),
                "UNREACHABLE",
                true,
            ),
            (
                Error::Decode {
                    context: "issue",
                    source: serde_json::from_str::<()>("not json").unwrap_err(),
                },
                "UNEXPECTED_RESPONSE",
                false,
            ),
            (
                Error::Config {
                    reason: "no default credential configured".to_string(),
                },
                "MISCONFIGURED",
                false,
            ),
            (
                Error::LimitExceeded {
                    what: "response bytes",
                    limit: 100,
                    actual: 200,
                },
                "LIMIT_EXCEEDED",
                false,
            ),
        ];

        for (error, expected_code, expected_retryable) in cases {
            let display = format!("{error}");
            let result = to_tool_error(error);
            assert_eq!(result.is_error, Some(true));
            assert_eq!(code_of(&result), expected_code, "for {display}");
            assert_eq!(retryable_of(&result), expected_retryable, "for {display}");

            let structured = result.structured_content.unwrap().to_string();
            assert!(
                !structured.contains("http://")
                    && !structured.contains("https://")
                    && !structured.contains("secret-path")
                    && !structured.contains("deadbeef"),
                "error envelope for {display} leaked a URL or token: {structured}"
            );
        }
    }
}

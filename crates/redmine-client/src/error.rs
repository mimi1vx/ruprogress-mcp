//! Typed errors for the Redmine client.

use std::time::Duration;

/// Errors returned by [`crate::client::RedmineClient`] and [`crate::client::Scoped`].
///
/// `#[non_exhaustive]`: new variants may be added in a minor version.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A transport-level failure (connect, timeout, TLS, ...). The
    /// underlying `reqwest::Error` has had its URL stripped before being
    /// stored, so it is safe to log.
    #[error("transport error")]
    Transport(#[source] reqwest::Error),
    /// Redmine responded with a non-2xx status this client does not map to
    /// a more specific variant.
    #[error("redmine returned {status}")]
    Api {
        /// The HTTP status code Redmine returned.
        status: http::StatusCode,
        /// Parsed `{"errors": [...]}` body, if any. Empty when the body was
        /// missing or failed to parse — the status is authoritative.
        errors: Vec<String>,
    },
    /// HTTP 401.
    #[error("unauthorized")]
    Unauthorized,
    /// HTTP 403.
    #[error("forbidden")]
    Forbidden,
    /// HTTP 404.
    #[error("not found")]
    NotFound,
    /// HTTP 429.
    #[error("rate limited")]
    RateLimited {
        /// Value of `Retry-After`, if present and parseable.
        retry_after: Option<Duration>,
    },
    /// The response body was not the JSON shape expected for `context`.
    #[error("failed to decode {context}")]
    Decode {
        /// What we were trying to decode, e.g. `"issue"`.
        context: &'static str,
        /// The underlying serde error.
        #[source]
        source: serde_json::Error,
    },
    /// A builder or configuration value was invalid.
    #[error("invalid configuration: {reason}")]
    Config {
        /// Human-readable reason.
        reason: String,
    },
    /// A caller-configured limit (page count, item count, response size) was
    /// exceeded.
    #[error("response exceeded limit: {actual} > {limit}")]
    LimitExceeded {
        /// What was limited, e.g. `"response bytes"`.
        what: &'static str,
        /// The configured limit.
        limit: u64,
        /// The observed value that exceeded it.
        actual: u64,
    },
    /// A non-2xx response from an OAuth token endpoint, shaped per RFC 6749
    /// §5.2 (`{"error", "error_description"}`) rather than Redmine's REST
    /// `{"errors": [...]}` shape [`from_status`] otherwise assumes.
    /// Produced only by [`crate::client::Scoped::exchange_authorization_code`].
    #[error("oauth error: {error}")]
    OAuth {
        /// The HTTP status Doorkeeper returned.
        status: http::StatusCode,
        /// The RFC 6749 `error` code, e.g. `"invalid_grant"`.
        error: String,
        /// The optional `error_description`.
        description: Option<String>,
    },
    /// A request or redirect target was refused because it did not match
    /// the configured Redmine origin — this client only ever sends
    /// credentials to the origin it was built against. Produced by
    /// [`crate::client::Scoped::download_attachment`] and by the
    /// client-wide redirect policy in [`crate::client::RedmineClientBuilder::build`].
    #[error("refused to send credentials to {origin}, which is not the configured Redmine origin")]
    ForeignOrigin {
        /// The refused origin, `scheme://host[:port]` only — never a path,
        /// query, or userinfo, since this value may end up in logs and in
        /// a model-visible message.
        origin: String,
    },
}

/// Convenience alias used throughout this crate.
pub type Result<T> = core::result::Result<T, Error>;

impl Error {
    /// Wrap a `reqwest::Error`, stripping its URL first so no token-bearing
    /// URL ever reaches a `Display`/`Debug` output or a log line.
    pub(crate) fn transport(source: reqwest::Error) -> Self {
        Self::Transport(source.without_url())
    }

    /// The HTTP status code this error carries, if any.
    #[must_use]
    pub fn status(&self) -> Option<http::StatusCode> {
        match self {
            Self::Api { status, .. } | Self::OAuth { status, .. } => Some(*status),
            Self::Unauthorized => Some(http::StatusCode::UNAUTHORIZED),
            Self::Forbidden => Some(http::StatusCode::FORBIDDEN),
            Self::NotFound => Some(http::StatusCode::NOT_FOUND),
            Self::RateLimited { .. } => Some(http::StatusCode::TOO_MANY_REQUESTS),
            Self::Transport(_)
            | Self::Decode { .. }
            | Self::Config { .. }
            | Self::LimitExceeded { .. }
            | Self::ForeignOrigin { .. } => None,
        }
    }

    /// Whether retrying the request that produced this error could plausibly
    /// succeed. Callers still must only retry idempotent verbs; see
    /// [`crate::retry`].
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } => true,
            Self::Api { status, .. } => status.is_server_error(),
            Self::Transport(source) => source.is_timeout() || source.is_connect(),
            Self::Unauthorized
            | Self::Forbidden
            | Self::NotFound
            | Self::Decode { .. }
            | Self::Config { .. }
            | Self::LimitExceeded { .. }
            | Self::ForeignOrigin { .. }
            // POST is never retried by `retry::method_is_retryable` anyway
            // (the only verb this variant is ever produced for), so this is
            // a documentation of that fact rather than a load-bearing check.
            | Self::OAuth { .. } => false,
        }
    }
}

/// Shape of a Redmine 422 error body: `{"errors": ["Subject can't be blank"]}`.
#[derive(serde::Deserialize)]
pub(crate) struct ErrorBody {
    #[serde(default)]
    pub errors: Vec<String>,
}

/// Shape of an RFC 6749 §5.2 OAuth error body: `{"error": "invalid_grant",
/// "error_description": "..."}`.
#[derive(serde::Deserialize)]
struct OAuthErrorBody {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Map a non-2xx status plus response body from an OAuth token endpoint to
/// [`Error::OAuth`]. A body that does not parse as [`OAuthErrorBody`] still
/// yields `Error::OAuth` (with a generic `error` code) rather than silently
/// falling back to [`Error::Api`]'s `{"errors": [...]}` shape, which
/// Doorkeeper's OAuth endpoints never use.
pub(crate) fn oauth_error_from_status(status: http::StatusCode, body: &[u8]) -> Error {
    match serde_json::from_slice::<OAuthErrorBody>(body) {
        Ok(parsed) => Error::OAuth {
            status,
            error: parsed.error,
            description: parsed.error_description,
        },
        Err(_) => Error::OAuth {
            status,
            error: "unknown_error".to_string(),
            description: None,
        },
    }
}

/// Map a non-2xx status plus response body to the right [`Error`] variant.
/// A failure to parse `body` as [`ErrorBody`] must not mask the status: it
/// falls back to `Api { status, errors: vec![] }`.
pub(crate) fn from_status(
    status: http::StatusCode,
    body: &[u8],
    retry_after: Option<Duration>,
) -> Error {
    match status {
        http::StatusCode::UNAUTHORIZED => Error::Unauthorized,
        http::StatusCode::FORBIDDEN => Error::Forbidden,
        http::StatusCode::NOT_FOUND => Error::NotFound,
        http::StatusCode::TOO_MANY_REQUESTS => Error::RateLimited { retry_after },
        _ => {
            let errors = serde_json::from_slice::<ErrorBody>(body)
                .map(|b| b.errors)
                .unwrap_or_default();
            Error::Api { status, errors }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn maps_401_403_404_429() {
        assert!(matches!(
            from_status(http::StatusCode::UNAUTHORIZED, b"", None),
            Error::Unauthorized
        ));
        assert!(matches!(
            from_status(http::StatusCode::FORBIDDEN, b"", None),
            Error::Forbidden
        ));
        assert!(matches!(
            from_status(http::StatusCode::NOT_FOUND, b"", None),
            Error::NotFound
        ));
        assert!(matches!(
            from_status(http::StatusCode::TOO_MANY_REQUESTS, b"", Some(Duration::from_secs(2))),
            Error::RateLimited {
                retry_after: Some(d)
            } if d == Duration::from_secs(2)
        ));
    }

    #[test]
    fn maps_422_with_errors_body() {
        let body = br#"{"errors":["Subject can't be blank"]}"#;
        match from_status(http::StatusCode::UNPROCESSABLE_ENTITY, body, None) {
            Error::Api { status, errors } => {
                assert_eq!(status, http::StatusCode::UNPROCESSABLE_ENTITY);
                assert_eq!(errors, vec!["Subject can't be blank".to_string()]);
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[test]
    fn maps_500_falls_back_to_empty_errors_on_unparseable_body() {
        match from_status(http::StatusCode::INTERNAL_SERVER_ERROR, b"not json", None) {
            Error::Api { status, errors } => {
                assert_eq!(status, http::StatusCode::INTERNAL_SERVER_ERROR);
                assert!(errors.is_empty());
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn transport_error_display_and_debug_contain_no_url() {
        // A body-decode failure carries the full request URL (path + query)
        // on `reqwest::Error`, unlike a connect failure whose source chain
        // only ever contains the resolved socket address. Use it to prove
        // `?key=<token>` never survives `Error::transport`.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/secret-path"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let url = format!("{}/secret-path?key=deadbeef-token", server.uri());

        let reqwest_err = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .expect("request should reach the mock server")
            .json::<serde_json::Value>()
            .await
            .expect_err("body is not valid JSON");
        assert!(
            reqwest_err.url().is_some(),
            "test premise: reqwest::Error must carry a URL before stripping"
        );

        let err = Error::transport(reqwest_err);
        let display = format!("{err}");
        let debug = format!("{err:?}");
        assert!(
            !display.contains("secret-path"),
            "Display leaked URL: {display}"
        );
        assert!(!debug.contains("secret-path"), "Debug leaked URL: {debug}");
        assert!(
            !debug.contains("deadbeef-token"),
            "Debug leaked query token: {debug}"
        );
    }
}

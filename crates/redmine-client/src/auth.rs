//! Per-request credentials.
//!
//! Redmine accepts the API key as a `key=` query parameter, but this crate
//! never sends it that way: query strings end up in server logs, proxy logs,
//! and `reqwest::Error`'s `Display`. The API key always goes in the
//! `X-Redmine-API-Key` header.

use secrecy::{ExposeSecret as _, SecretString};

/// A single Redmine identity. Cheap to clone; the secret itself is
/// reference-counted-free (each clone re-copies the string, same as
/// `SecretString`'s own `Clone` impl).
#[derive(Clone)]
pub enum Credential {
    /// `X-Redmine-API-Key: <key>`.
    ApiKey(SecretString),
    /// HTTP Basic auth.
    Basic {
        /// Username.
        user: String,
        /// Password.
        pass: SecretString,
    },
    /// `Authorization: Bearer <token>` (`OAuth2` access token).
    Bearer(SecretString),
}

impl Credential {
    /// Attach this credential to `req`. The header value is marked
    /// `set_sensitive(true)` so it is redacted by `http`'s own `Debug` and by
    /// `tower-http`'s trace layer.
    pub(crate) fn apply(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            Self::ApiKey(key) => {
                let mut value = http::HeaderValue::try_from(key.expose_secret())
                    .unwrap_or_else(|_| http::HeaderValue::from_static("invalid-api-key"));
                value.set_sensitive(true);
                req.header("X-Redmine-API-Key", value)
            }
            Self::Basic { user, pass } => req.basic_auth(user, Some(pass.expose_secret())),
            Self::Bearer(token) => req.bearer_auth(token.expose_secret()),
        }
    }
}

impl core::fmt::Debug for Credential {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ApiKey(_) => write!(f, "Credential::ApiKey(<redacted>)"),
            Self::Basic { user, .. } => {
                write!(
                    f,
                    "Credential::Basic {{ user: {user:?}, pass: <redacted> }}"
                )
            }
            Self::Bearer(_) => write!(f, "Credential::Bearer(<redacted>)"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn debug_redacts_all_variants() {
        let api_key = Credential::ApiKey(SecretString::from("super-secret-key"));
        let basic = Credential::Basic {
            user: "alice".to_string(),
            pass: SecretString::from("hunter2"),
        };
        let bearer = Credential::Bearer(SecretString::from("oauth-token-xyz"));

        assert_eq!(format!("{api_key:?}"), "Credential::ApiKey(<redacted>)");
        assert_eq!(
            format!("{basic:?}"),
            "Credential::Basic { user: \"alice\", pass: <redacted> }"
        );
        assert_eq!(format!("{bearer:?}"), "Credential::Bearer(<redacted>)");

        for secret in ["super-secret-key", "hunter2", "oauth-token-xyz"] {
            assert!(!format!("{api_key:?}").contains(secret));
            assert!(!format!("{basic:?}").contains(secret));
            assert!(!format!("{bearer:?}").contains(secret));
        }
    }

    #[tokio::test]
    async fn api_key_sets_x_redmine_api_key_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .and(header_exists("X-Redmine-API-Key"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let cred = Credential::ApiKey(SecretString::from("abc123"));
        let req = cred.apply(reqwest::Client::new().get(format!("{}/x", server.uri())));
        let resp = req.send().await.expect("request should succeed");
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn basic_sets_authorization_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .and(header_exists("Authorization"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let cred = Credential::Basic {
            user: "alice".to_string(),
            pass: SecretString::from("hunter2"),
        };
        let req = cred.apply(reqwest::Client::new().get(format!("{}/x", server.uri())));
        let resp = req.send().await.expect("request should succeed");
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn bearer_sets_authorization_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .and(header_exists("Authorization"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let cred = Credential::Bearer(SecretString::from("oauth-token-xyz"));
        let req = cred.apply(reqwest::Client::new().get(format!("{}/x", server.uri())));
        let resp = req.send().await.expect("request should succeed");
        assert_eq!(resp.status(), 200);
    }
}

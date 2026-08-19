//! `POST /oauth/token` (RFC 6749 §5.1) response, consumed by `oauth-proxy`
//! mode's upstream authorization-code exchange
//! ([`crate::client::Scoped::exchange_authorization_code`]).

use secrecy::SecretString;
use serde::Deserialize;

/// The wire shape: plain `String`s. `secrecy`'s `serde` support is a
/// separate cargo feature this workspace does not otherwise enable, so the
/// two token fields are wrapped by hand in `From<RawOAuthToken>` below
/// rather than pulling in a feature for two fields.
#[derive(Deserialize)]
struct RawOAuthToken {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
}

/// An RFC 6749 §5.1 access token response. Permissive by design (only
/// `access_token` is required, no `deny_unknown_fields`): Doorkeeper's exact
/// field set is not part of any contract this crate can rely on — same
/// reasoning as [`crate::model::introspection::Introspection`].
///
/// `Debug` is hand-written and redacts both token fields: unlike
/// `Introspection`, this type carries live, usable credentials.
#[non_exhaustive]
#[derive(Clone, Deserialize)]
#[serde(from = "RawOAuthToken")]
pub struct OAuthToken {
    /// The upstream access token to store and later present to Redmine.
    pub access_token: SecretString,
    /// The upstream refresh token, if Doorkeeper's `use_refresh_token`
    /// setting is enabled.
    pub refresh_token: Option<SecretString>,
    /// Upstream lifetime in seconds, if reported.
    pub expires_in: Option<u64>,
    /// Space-delimited granted scopes, if reported. Absent means "whatever
    /// was requested was granted" per RFC 6749 §5.1.
    pub scope: Option<String>,
    /// The token type, e.g. `"Bearer"`.
    pub token_type: Option<String>,
}

impl From<RawOAuthToken> for OAuthToken {
    fn from(raw: RawOAuthToken) -> Self {
        Self {
            access_token: SecretString::from(raw.access_token),
            refresh_token: raw.refresh_token.map(SecretString::from),
            expires_in: raw.expires_in,
            scope: raw.scope,
            token_type: raw.token_type,
        }
    }
}

impl std::fmt::Debug for OAuthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthToken")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_in", &self.expires_in)
            .field("scope", &self.scope)
            .field("token_type", &self.token_type)
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use secrecy::ExposeSecret;

    use super::*;

    #[test]
    fn deserializes_a_minimal_response() {
        let token: OAuthToken =
            serde_json::from_str(r#"{"access_token":"abc123"}"#).expect("valid json");
        assert_eq!(token.access_token.expose_secret(), "abc123");
        assert!(token.refresh_token.is_none());
        assert!(token.expires_in.is_none());
    }

    #[test]
    fn deserializes_a_full_response_and_ignores_unknown_fields() {
        let token: OAuthToken = serde_json::from_str(
            r#"{
                "access_token": "abc123",
                "refresh_token": "def456",
                "expires_in": 7200,
                "scope": "view_issues edit_issues",
                "token_type": "Bearer",
                "some_future_field": "ignored"
            }"#,
        )
        .expect("valid json");
        assert_eq!(token.access_token.expose_secret(), "abc123");
        assert_eq!(
            token
                .refresh_token
                .as_ref()
                .map(ExposeSecret::expose_secret),
            Some("def456")
        );
        assert_eq!(token.expires_in, Some(7200));
        assert_eq!(token.scope.as_deref(), Some("view_issues edit_issues"));
    }

    #[test]
    fn debug_never_contains_either_token() {
        let token: OAuthToken = serde_json::from_str(
            r#"{"access_token":"super-secret-access","refresh_token":"super-secret-refresh"}"#,
        )
        .expect("valid json");
        let rendered = format!("{token:?}");
        assert!(!rendered.contains("super-secret-access"));
        assert!(!rendered.contains("super-secret-refresh"));
        assert!(rendered.contains("redacted"));
    }
}

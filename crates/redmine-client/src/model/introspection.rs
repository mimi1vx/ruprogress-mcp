//! `POST /oauth/introspect` (RFC 7662).

use serde::Deserialize;

/// An RFC 7662 introspection response. Permissive by design (every field but
/// `active` is `Option`, no `deny_unknown_fields`): Doorkeeper's exact field
/// set is not part of any contract this crate can rely on.
///
/// Holds no secret, so a derived `Debug` is fine — see the `debug_has_no_manual_impl`
/// test, which exists precisely so a future field addition has to think about
/// that before assuming it.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct Introspection {
    /// Whether the token is currently active.
    pub active: bool,
    /// Space-delimited scope string, if the token carries one.
    #[serde(default)]
    pub scope: Option<String>,
    /// The OAuth client the token was issued to.
    #[serde(default)]
    pub client_id: Option<String>,
    /// The resource owner's username, if the token is user-bound.
    #[serde(default)]
    pub username: Option<String>,
    /// The subject identifier (resource owner), if present.
    #[serde(default)]
    pub sub: Option<String>,
    /// Expiry, as Unix seconds.
    #[serde(default)]
    pub exp: Option<i64>,
    /// Issued-at, as Unix seconds.
    #[serde(default)]
    pub iat: Option<i64>,
    /// The token type, e.g. `"Bearer"`.
    #[serde(default)]
    pub token_type: Option<String>,
}

impl Introspection {
    /// Split [`Self::scope`] on ASCII whitespace, per RFC 7662's
    /// space-delimited scope string. Empty when `scope` is `None` or empty.
    #[must_use]
    pub fn scopes(&self) -> Vec<&str> {
        self.scope
            .as_deref()
            .map(|s| s.split_ascii_whitespace().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn debug_has_no_manual_impl_and_holds_no_secret() {
        // A canary, not a security control: `Introspection` carries no
        // secret today, so the derived `Debug` is safe. If a future field
        // addition changes that, this test's assertion (not its existence)
        // must be revisited alongside a manual `Debug`.
        let value = Introspection {
            active: true,
            scope: Some("view_issues edit_issues".to_string()),
            client_id: Some("client-1".to_string()),
            username: Some("alice".to_string()),
            sub: Some("5".to_string()),
            exp: Some(1_000_000),
            iat: Some(999_000),
            token_type: Some("Bearer".to_string()),
        };
        let rendered = format!("{value:?}");
        assert!(rendered.contains("active"));
    }

    #[test]
    fn scopes_splits_on_whitespace() {
        let value = Introspection {
            active: true,
            scope: Some("view_issues  edit_issues\tadd_issue_notes".to_string()),
            client_id: None,
            username: None,
            sub: None,
            exp: None,
            iat: None,
            token_type: None,
        };
        assert_eq!(
            value.scopes(),
            vec!["view_issues", "edit_issues", "add_issue_notes"]
        );
    }

    #[test]
    fn scopes_empty_when_scope_is_none() {
        let value = Introspection {
            active: false,
            scope: None,
            client_id: None,
            username: None,
            sub: None,
            exp: None,
            iat: None,
            token_type: None,
        };
        assert!(value.scopes().is_empty());
    }

    #[test]
    fn deserializes_a_minimal_inactive_response() {
        let value: Introspection = serde_json::from_str(r#"{"active":false}"#).unwrap();
        assert!(!value.active);
        assert!(value.scope.is_none());
    }

    #[test]
    fn deserializes_a_full_active_response_and_ignores_unknown_fields() {
        let value: Introspection = serde_json::from_str(
            r#"{
                "active": true,
                "scope": "view_issues edit_issues",
                "client_id": "abc",
                "username": "alice",
                "sub": "5",
                "exp": 1999999999,
                "iat": 1999999000,
                "token_type": "Bearer",
                "some_future_field": "ignored"
            }"#,
        )
        .unwrap();
        assert!(value.active);
        assert_eq!(value.username.as_deref(), Some("alice"));
        assert_eq!(value.exp, Some(1_999_999_999));
    }
}

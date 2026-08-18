//! RFC 9728 protected-resource metadata and RFC 8414 authorization-server
//! metadata (D3): pure functions of `&Config`, rendered per request rather
//! than memoised (D8) since the set is at most a few dozen short strings.
//!
//! Redmine's Doorkeeper serves neither document itself; this module renders
//! them on Redmine's behalf, naming Redmine's real `/oauth/authorize`,
//! `/oauth/token`, and `/oauth/revoke` endpoints.

use serde_json::{Value, json};

use crate::config::{AuthMode, Config, DiscoveryAs, OAuthConfig};

/// The name every discovery document identifies this resource server as.
const RESOURCE_NAME: &str = "ruprogress-mcp";

/// Which shape of authorization-server metadata to render (C9): `oauth`
/// names Redmine's own endpoints, `oauth-proxy` names this server's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiscoveryMode {
    Oauth,
    OAuthProxy,
}

impl DiscoveryMode {
    /// The mode and shared resource config for whichever bearer-token auth
    /// mode `auth` is, or `None` for a mode with no discovery documents at
    /// all (`legacy`, `legacy-per-user`).
    pub(crate) fn from_auth(auth: &AuthMode) -> Option<(Self, &OAuthConfig)> {
        match auth {
            AuthMode::OAuth(oauth) => Some((Self::Oauth, oauth)),
            AuthMode::OAuthProxy(proxy) => Some((Self::OAuthProxy, &proxy.resource)),
            AuthMode::Legacy { .. } | AuthMode::LegacyPerUser { .. } => None,
        }
    }
}

/// Strips a single trailing `/`, matching `auth::oauth::Challenge`'s own
/// normalization of `REDMINE_MCP_BASE_URL`/`REDMINE_URL`.
fn trim_trailing_slash(raw: &str) -> &str {
    raw.strip_suffix('/').unwrap_or(raw)
}

/// The authorization-server `issuer` this deployment advertises (D3):
/// Redmine's own URL in the default `redmine` discovery mode, or this
/// server's own public base URL in `self` mode.
fn issuer(config: &Config, oauth: &OAuthConfig) -> String {
    match oauth.discovery_as {
        DiscoveryAs::Redmine => trim_trailing_slash(config.redmine.url.as_str()).to_string(),
        DiscoveryAs::SelfHosted => trim_trailing_slash(oauth.base_url.as_str()).to_string(),
    }
}

/// RFC 9728 §3.1 protected-resource metadata, served at
/// `/.well-known/oauth-protected-resource{mcp_path}`.
///
/// `oauth` and `mcp_path` are supplied by the caller rather than
/// re-derived from `config.auth`/`config.transport`: both are already known
/// wherever this is called (only ever from an oauth-shaped route), so there
/// is nothing here to fall back on if they were absent.
pub(crate) fn protected_resource(config: &Config, oauth: &OAuthConfig, mcp_path: &str) -> Value {
    let base = trim_trailing_slash(oauth.base_url.as_str());
    json!({
        "resource": format!("{base}{mcp_path}"),
        "authorization_servers": [issuer(config, oauth)],
        "scopes_supported": oauth.scopes,
        "bearer_methods_supported": ["header"],
        "resource_name": RESOURCE_NAME,
    })
}

/// RFC 8414 authorization-server metadata (C9). In `oauth` mode, served at
/// the suffixed well-known path in `redmine` discovery mode or at the root
/// well-known path in `self` mode — never both; see
/// `transport::http::router`. In `oauth-proxy` mode, always served at the
/// root path (P12) naming *this* server's own endpoints, since this server
/// is the authorization server in that mode.
pub(crate) fn authorization_server(
    config: &Config,
    oauth: &OAuthConfig,
    mode: DiscoveryMode,
) -> Value {
    let issuer = issuer(config, oauth);
    match mode {
        DiscoveryMode::Oauth => {
            let redmine_base = trim_trailing_slash(config.redmine.url.as_str());
            json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{redmine_base}/oauth/authorize"),
                "token_endpoint": format!("{redmine_base}/oauth/token"),
                "revocation_endpoint": format!("{redmine_base}/oauth/revoke"),
                "response_types_supported": ["code"],
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
                "scopes_supported": oauth.scopes,
            })
        }
        DiscoveryMode::OAuthProxy => {
            let base = trim_trailing_slash(oauth.base_url.as_str());
            json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": format!("{base}/token"),
                "registration_endpoint": format!("{base}/register"),
                "revocation_endpoint": format!("{base}/revoke"),
                "response_types_supported": ["code"],
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"],
                "authorization_response_iss_parameter_supported": true,
                "scopes_supported": oauth.scopes,
            })
        }
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
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::TransportKind;

    fn config(extra: &[(&str, &str)]) -> Config {
        let mut vars: BTreeMap<String, String> = BTreeMap::from([
            (
                "REDMINE_URL".to_string(),
                "https://redmine.example.com".to_string(),
            ),
            ("REDMINE_AUTH_MODE".to_string(), "oauth".to_string()),
            (
                "REDMINE_MCP_BASE_URL".to_string(),
                "https://mcp.example.com".to_string(),
            ),
            (
                "REDMINE_INTROSPECT_CLIENT_ID".to_string(),
                "introspect-client".to_string(),
            ),
            (
                "REDMINE_INTROSPECT_CLIENT_SECRET".to_string(),
                "introspect-secret".to_string(),
            ),
        ]);
        for (k, v) in extra {
            vars.insert((*k).to_string(), (*v).to_string());
        }
        Config::from_map(&vars, TransportKind::Http).expect("valid oauth config")
    }

    /// `oauth_resource()` and the HTTP mcp path, unwrapped: every test config
    /// here is `oauth` mode on the HTTP transport, so both are always `Some`.
    fn oauth_and_path(cfg: &Config) -> (&OAuthConfig, &str) {
        (
            cfg.oauth_resource().expect("oauth mode"),
            cfg.transport
                .as_http()
                .expect("http transport")
                .mcp_path
                .as_str(),
        )
    }

    #[test]
    fn protected_resource_names_this_servers_mcp_url_as_the_resource() {
        let cfg = config(&[]);
        let (oauth, mcp_path) = oauth_and_path(&cfg);
        let doc = protected_resource(&cfg, oauth, mcp_path);
        assert_eq!(doc["resource"], "https://mcp.example.com/mcp");
        assert_eq!(
            doc["authorization_servers"],
            json!(["https://redmine.example.com"])
        );
        assert_eq!(doc["bearer_methods_supported"], json!(["header"]));
    }

    #[test]
    fn authorization_server_points_at_redmine_by_default() {
        let cfg = config(&[]);
        let (oauth, _) = oauth_and_path(&cfg);
        let doc = authorization_server(&cfg, oauth, DiscoveryMode::Oauth);
        assert_eq!(doc["issuer"], "https://redmine.example.com");
        assert_eq!(
            doc["authorization_endpoint"],
            "https://redmine.example.com/oauth/authorize"
        );
        assert_eq!(
            doc["token_endpoint"],
            "https://redmine.example.com/oauth/token"
        );
        assert_eq!(
            doc["revocation_endpoint"],
            "https://redmine.example.com/oauth/revoke"
        );
    }

    #[test]
    fn self_discovery_mode_uses_the_base_url_as_issuer_but_keeps_redmine_endpoints() {
        let cfg = config(&[("REDMINE_OAUTH_DISCOVERY_AS", "self")]);
        let (oauth, mcp_path) = oauth_and_path(&cfg);
        let prm = protected_resource(&cfg, oauth, mcp_path);
        let as_doc = authorization_server(&cfg, oauth, DiscoveryMode::Oauth);
        assert_eq!(as_doc["issuer"], "https://mcp.example.com");
        assert_eq!(
            prm["authorization_servers"],
            json!(["https://mcp.example.com"])
        );
        // Authorize/token/revoke still go directly to Redmine either way.
        assert_eq!(
            as_doc["authorization_endpoint"],
            "https://redmine.example.com/oauth/authorize"
        );
    }

    #[test]
    fn scopes_supported_is_identical_in_both_documents() {
        let cfg = config(&[]);
        let (oauth, mcp_path) = oauth_and_path(&cfg);
        let prm = protected_resource(&cfg, oauth, mcp_path);
        let as_doc = authorization_server(&cfg, oauth, DiscoveryMode::Oauth);
        assert_eq!(prm["scopes_supported"], as_doc["scopes_supported"]);
        assert!(prm["scopes_supported"].as_array().unwrap().len() > 1);
    }

    #[test]
    fn neither_document_leaks_the_introspection_client_or_a_secret() {
        let cfg = config(&[]);
        let (oauth, mcp_path) = oauth_and_path(&cfg);
        for doc in [
            protected_resource(&cfg, oauth, mcp_path),
            authorization_server(&cfg, oauth, DiscoveryMode::Oauth),
        ] {
            let rendered = doc.to_string();
            assert!(!rendered.contains("introspect-client"));
            assert!(!rendered.contains("introspect-secret"));
        }
    }

    #[test]
    fn mcp_path_is_respected_in_the_protected_resource_document() {
        let cfg = config(&[("FASTMCP_STREAMABLE_HTTP_PATH", "/api/mcp")]);
        let (oauth, mcp_path) = oauth_and_path(&cfg);
        let doc = protected_resource(&cfg, oauth, mcp_path);
        assert_eq!(doc["resource"], "https://mcp.example.com/api/mcp");
    }

    // --- oauth-proxy --------------------------------------------------------

    fn proxy_config(extra: &[(&str, &str)]) -> Config {
        let mut vars: BTreeMap<String, String> = BTreeMap::from([
            (
                "REDMINE_URL".to_string(),
                "https://redmine.example.com".to_string(),
            ),
            ("REDMINE_AUTH_MODE".to_string(), "oauth-proxy".to_string()),
            (
                "REDMINE_MCP_BASE_URL".to_string(),
                "https://mcp.example.com".to_string(),
            ),
            (
                "REDMINE_INTROSPECT_CLIENT_ID".to_string(),
                "introspect-client".to_string(),
            ),
            (
                "REDMINE_INTROSPECT_CLIENT_SECRET".to_string(),
                "introspect-secret".to_string(),
            ),
        ]);
        for (k, v) in extra {
            vars.insert((*k).to_string(), (*v).to_string());
        }
        Config::from_map(&vars, TransportKind::Http).expect("valid oauth-proxy config")
    }

    #[test]
    fn discovery_mode_from_auth_selects_oauth_proxy() {
        let cfg = proxy_config(&[]);
        let (mode, oauth) = DiscoveryMode::from_auth(&cfg.auth).expect("proxy mode has a resource");
        assert_eq!(mode, DiscoveryMode::OAuthProxy);
        assert_eq!(oauth.base_url.as_str(), "https://mcp.example.com/");
    }

    #[test]
    fn proxy_protected_resource_names_this_server_as_its_own_authorization_server() {
        let cfg = proxy_config(&[]);
        let (oauth, mcp_path) = oauth_and_path(&cfg);
        let doc = protected_resource(&cfg, oauth, mcp_path);
        assert_eq!(
            doc["authorization_servers"],
            json!(["https://mcp.example.com"])
        );
    }

    #[test]
    fn proxy_authorization_server_names_this_servers_own_endpoints() {
        let cfg = proxy_config(&[]);
        let (oauth, _) = oauth_and_path(&cfg);
        let doc = authorization_server(&cfg, oauth, DiscoveryMode::OAuthProxy);
        assert_eq!(doc["issuer"], "https://mcp.example.com");
        assert_eq!(
            doc["authorization_endpoint"],
            "https://mcp.example.com/authorize"
        );
        assert_eq!(doc["token_endpoint"], "https://mcp.example.com/token");
        assert_eq!(
            doc["registration_endpoint"],
            "https://mcp.example.com/register"
        );
        assert_eq!(doc["revocation_endpoint"], "https://mcp.example.com/revoke");
        assert_eq!(
            doc["token_endpoint_auth_methods_supported"],
            json!(["none"])
        );
        assert_eq!(doc["code_challenge_methods_supported"], json!(["S256"]));
        assert_eq!(doc["authorization_response_iss_parameter_supported"], true);
    }

    #[test]
    fn proxy_scopes_supported_matches_the_protected_resource_document() {
        let cfg = proxy_config(&[]);
        let (oauth, mcp_path) = oauth_and_path(&cfg);
        let prm = protected_resource(&cfg, oauth, mcp_path);
        let as_doc = authorization_server(&cfg, oauth, DiscoveryMode::OAuthProxy);
        assert_eq!(prm["scopes_supported"], as_doc["scopes_supported"]);
    }

    #[test]
    fn proxy_documents_never_leak_the_introspection_client_or_a_secret() {
        let cfg = proxy_config(&[]);
        let (oauth, mcp_path) = oauth_and_path(&cfg);
        for doc in [
            protected_resource(&cfg, oauth, mcp_path),
            authorization_server(&cfg, oauth, DiscoveryMode::OAuthProxy),
        ] {
            let rendered = doc.to_string();
            assert!(!rendered.contains("introspect-client"));
            assert!(!rendered.contains("introspect-secret"));
        }
    }
}

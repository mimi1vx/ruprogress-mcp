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

fn oauth_config(config: &Config) -> &OAuthConfig {
    let AuthMode::OAuth(oauth) = &config.auth else {
        unreachable!("oauth::metadata is only ever rendered in AuthMode::OAuth")
    };
    oauth
}

fn mcp_path(config: &Config) -> &str {
    config.transport.as_http().map_or_else(
        || unreachable!("oauth mode requires the HTTP transport"),
        |http| http.mcp_path.as_str(),
    )
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
pub(crate) fn protected_resource(config: &Config) -> Value {
    let oauth = oauth_config(config);
    let base = trim_trailing_slash(oauth.base_url.as_str());
    json!({
        "resource": format!("{base}{}", mcp_path(config)),
        "authorization_servers": [issuer(config, oauth)],
        "scopes_supported": oauth.scopes,
        "bearer_methods_supported": ["header"],
        "resource_name": RESOURCE_NAME,
    })
}

/// RFC 8414 authorization-server metadata, served at the suffixed
/// well-known path in `redmine` discovery mode or at the root well-known
/// path in `self` mode (D3) — never both; see `transport::http::router`.
pub(crate) fn authorization_server(config: &Config) -> Value {
    let oauth = oauth_config(config);
    let redmine_base = trim_trailing_slash(config.redmine.url.as_str());
    json!({
        "issuer": issuer(config, oauth),
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

    #[test]
    fn protected_resource_names_this_servers_mcp_url_as_the_resource() {
        let doc = protected_resource(&config(&[]));
        assert_eq!(doc["resource"], "https://mcp.example.com/mcp");
        assert_eq!(
            doc["authorization_servers"],
            json!(["https://redmine.example.com"])
        );
        assert_eq!(doc["bearer_methods_supported"], json!(["header"]));
    }

    #[test]
    fn authorization_server_points_at_redmine_by_default() {
        let doc = authorization_server(&config(&[]));
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
        let prm = protected_resource(&cfg);
        let as_doc = authorization_server(&cfg);
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
        let prm = protected_resource(&cfg);
        let as_doc = authorization_server(&cfg);
        assert_eq!(prm["scopes_supported"], as_doc["scopes_supported"]);
        assert!(prm["scopes_supported"].as_array().unwrap().len() > 1);
    }

    #[test]
    fn neither_document_leaks_the_introspection_client_or_a_secret() {
        let cfg = config(&[]);
        for doc in [protected_resource(&cfg), authorization_server(&cfg)] {
            let rendered = doc.to_string();
            assert!(!rendered.contains("introspect-client"));
            assert!(!rendered.contains("introspect-secret"));
        }
    }

    #[test]
    fn mcp_path_is_respected_in_the_protected_resource_document() {
        let cfg = config(&[("FASTMCP_STREAMABLE_HTTP_PATH", "/api/mcp")]);
        let doc = protected_resource(&cfg);
        assert_eq!(doc["resource"], "https://mcp.example.com/api/mcp");
    }
}

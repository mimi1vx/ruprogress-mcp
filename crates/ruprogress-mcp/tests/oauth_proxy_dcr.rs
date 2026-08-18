//! End-to-end `AuthMode::OAuthProxy`: discovery documents that name this
//! server as its own authorization server, `POST /register` (RFC 7591), and
//! the route matrix — over the real HTTP router. The full authorization-code
//! flow through `/authorize`/`/auth/callback`/`/token` is covered in
//! `tests/oauth_proxy_flow.rs`; here `/mcp` still rejects every request with
//! the `401` challenge unless it carries a valid `rup_at_` proxy token.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use reqwest::StatusCode;
use serde_json::Value;

fn oauth_proxy_env(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut env = vec![
        ("REDMINE_AUTH_MODE", "oauth-proxy"),
        ("REDMINE_MCP_BASE_URL", "http://localhost:3040"),
        ("REDMINE_INTROSPECT_CLIENT_ID", "introspect-client"),
        ("REDMINE_INTROSPECT_CLIENT_SECRET", "introspect-secret"),
    ];
    env.extend_from_slice(extra);
    env
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .expect("build a test HTTP client")
}

// --- discovery documents (P12, C9) -----------------------------------------

#[tokio::test]
async fn protected_resource_names_this_server_as_its_own_authorization_server() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    let response = client()
        .get(harness.url("/.well-known/oauth-protected-resource/mcp"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["resource"], "http://localhost:3040/mcp");
    assert_eq!(
        body["authorization_servers"],
        serde_json::json!(["http://localhost:3040"])
    );
}

#[tokio::test]
async fn authorization_server_document_is_served_at_the_root_and_the_suffixed_path_404s() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;

    let root = client()
        .get(harness.url("/.well-known/oauth-authorization-server"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(root.status(), StatusCode::OK);
    let body: Value = root.json().await.expect("json body");
    assert_eq!(body["issuer"], "http://localhost:3040");
    assert_eq!(
        body["authorization_endpoint"],
        "http://localhost:3040/authorize"
    );
    assert_eq!(body["token_endpoint"], "http://localhost:3040/token");
    assert_eq!(
        body["registration_endpoint"],
        "http://localhost:3040/register"
    );
    assert_eq!(body["revocation_endpoint"], "http://localhost:3040/revoke");
    assert_eq!(
        body["token_endpoint_auth_methods_supported"],
        serde_json::json!(["none"])
    );
    assert_eq!(
        body["code_challenge_methods_supported"],
        serde_json::json!(["S256"])
    );
    assert_eq!(body["authorization_response_iss_parameter_supported"], true);

    let suffixed = client()
        .get(harness.url("/.well-known/oauth-authorization-server/mcp"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(suffixed.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn explicit_redmine_discovery_is_rejected_at_boot() {
    // `support::http_harness` panics on a `Config::from_map` error inside
    // `build_server`, so a boot `Conflict` shows up as a panicked task; the
    // narrowest way to assert it here is via the pure `Config` API instead
    // of standing up a harness that can never come up.
    let mut vars: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::from([
        (
            "REDMINE_URL".to_string(),
            "https://redmine.example.com".to_string(),
        ),
        ("REDMINE_AUTH_MODE".to_string(), "oauth-proxy".to_string()),
        (
            "REDMINE_MCP_BASE_URL".to_string(),
            "http://localhost:3040".to_string(),
        ),
        (
            "REDMINE_INTROSPECT_CLIENT_ID".to_string(),
            "introspect-client".to_string(),
        ),
        (
            "REDMINE_INTROSPECT_CLIENT_SECRET".to_string(),
            "introspect-secret".to_string(),
        ),
        (
            "REDMINE_OAUTH_DISCOVERY_AS".to_string(),
            "redmine".to_string(),
        ),
    ]);
    let err = ruprogress_mcp::config::Config::from_map(
        &vars,
        ruprogress_mcp::config::TransportKind::Http,
    )
    .expect_err("redmine discovery should conflict with oauth-proxy");
    assert!(matches!(
        err,
        ruprogress_mcp::config::ConfigError::Conflict { .. }
    ));
    vars.remove("REDMINE_OAUTH_DISCOVERY_AS");
    ruprogress_mcp::config::Config::from_map(&vars, ruprogress_mcp::config::TransportKind::Http)
        .expect("unset should default to self-hosted discovery and succeed");
}

// --- POST /register (RFC 7591, C6) ------------------------------------------

#[tokio::test]
async fn register_returns_a_client_id_and_no_client_secret() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    let response = client()
        .post(harness.url("/register"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({ "redirect_uris": ["http://localhost:4000/cb"] }))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: Value = response.json().await.expect("json body");
    assert!(body["client_id"].as_str().is_some());
    assert!(body.get("client_secret").is_none());
    assert_eq!(body["token_endpoint_auth_method"], "none");
}

#[tokio::test]
async fn register_rejects_a_non_loopback_uri_by_default() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    let response = client()
        .post(harness.url("/register"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({ "redirect_uris": ["https://app.example.com/cb"] }))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_redirect_uri");
}

#[tokio::test]
async fn register_accepts_a_non_loopback_uri_once_the_allowlist_permits_it() {
    let harness = support::http_harness(&oauth_proxy_env(&[(
        "REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS",
        "https://app.example.com/*",
    )]))
    .await;
    let response = client()
        .post(harness.url("/register"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({ "redirect_uris": ["https://app.example.com/cb"] }))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn register_404s_in_oauth_and_legacy_modes() {
    for env in [
        vec![
            ("REDMINE_AUTH_MODE", "oauth"),
            ("REDMINE_MCP_BASE_URL", "http://localhost:3040"),
            ("REDMINE_INTROSPECT_CLIENT_ID", "introspect-client"),
            ("REDMINE_INTROSPECT_CLIENT_SECRET", "introspect-secret"),
        ],
        vec![],
    ] {
        let harness = support::http_harness(&env).await;
        let response = client()
            .post(harness.url("/register"))
            .header("content-type", "application/json")
            .json(&serde_json::json!({ "redirect_uris": ["http://localhost/cb"] }))
            .send()
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{env:?}");
    }
}

// --- route matrix -----------------------------------------------------------

#[tokio::test]
async fn authorize_and_token_are_mounted_and_reachable() {
    // `/authorize` and `/token` exist as of 6c2 (the full authorization-code
    // flow is exercised end to end in `tests/oauth_proxy_flow.rs`); this
    // test only pins that both routes are mounted with the right method.
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;

    // No query params at all is a Phase A failure (F1): a plain 400, never
    // a 404.
    let authorize = client()
        .get(harness.url("/authorize"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(authorize.status(), StatusCode::BAD_REQUEST);

    // `/token` is POST-only; a GET is a 405, not a 404.
    let token = client()
        .get(harness.url("/token"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(token.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn mcp_route_is_401_with_a_www_authenticate_challenge_with_no_token() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    let response = client()
        .post(harness.mcp_url())
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        response
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("oauth-protected-resource"))
    );
}

/// A token without the `rup_at_` prefix is never accepted at `/mcp` here —
/// not even one that would otherwise look plausible. This guards against
/// the specific regression of reusing `oauth` mode's `require_bearer`
/// unchanged, which would introspect (and could accept) a raw upstream
/// Redmine token here; see `tests/oauth_proxy_flow.rs` for the positive
/// case of a real proxy token driving `/mcp`.
#[tokio::test]
async fn mcp_route_is_401_even_with_a_bearer_token_present() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    let response = client()
        .post(harness.mcp_url())
        .header("content-type", "application/json")
        .header("authorization", "Bearer whatever-a-caller-sends")
        .body("{}")
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn livez_readyz_health_and_files_are_unauthenticated() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    for path in ["/livez", "/readyz", "/health"] {
        let response = client()
            .get(harness.url(path))
            .send()
            .await
            .expect("request should complete");
        assert_ne!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
    // An unknown uuid 404s rather than 401ing — the route itself never
    // checks a bearer token in any auth mode.
    let response = client()
        .get(harness.url("/files/00000000-0000-0000-0000-000000000000"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

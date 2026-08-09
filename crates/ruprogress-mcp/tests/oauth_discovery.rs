//! End-to-end phase 6b2: RFC 9728/8414 discovery documents, `POST /revoke`,
//! and the introspection readiness probe — over the real HTTP router,
//! against a wiremock Redmine that also stands in for Doorkeeper.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::items_after_statements
)]

mod support;

use reqwest::StatusCode;
use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::Value;
use wiremock::matchers::{basic_auth, body_string_contains, method, path};
use wiremock::{Mock, ResponseTemplate};

const CLIENT_ID: &str = "introspect-client";
const CLIENT_SECRET: &str = "introspect-secret";

fn oauth_env(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut env = vec![
        ("REDMINE_AUTH_MODE", "oauth"),
        ("REDMINE_MCP_BASE_URL", "http://localhost:3040"),
        ("REDMINE_INTROSPECT_CLIENT_ID", CLIENT_ID),
        ("REDMINE_INTROSPECT_CLIENT_SECRET", CLIENT_SECRET),
    ];
    env.extend_from_slice(extra);
    env
}

fn raw_client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .expect("build a test HTTP client")
}

async fn connect_with_token(
    harness: &support::HttpHarness,
    token: &str,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let config = StreamableHttpClientTransportConfig::with_uri(harness.mcp_url())
        .auth_header(token.to_string());
    let transport = StreamableHttpClientTransport::from_config(config);
    ().serve(transport)
        .await
        .expect("client with a valid bearer token should connect")
}

async fn mock_current_user(redmine: &wiremock::MockServer, token: &str) {
    Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .and(wiremock::matchers::header(
            "authorization",
            format!("Bearer {token}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "user": {
                "id": 5, "login": "alice", "firstname": "Alice",
                "lastname": "Example", "mail": "alice@example.com",
                "created_on": "2024-01-01T00:00:00Z",
            }
        })))
        .mount(redmine)
        .await;
}

// --- discovery documents (D3, D6) ------------------------------------------

#[tokio::test]
async fn protected_resource_document_is_served_without_a_token() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    let response = raw_client()
        .get(harness.url("/.well-known/oauth-protected-resource/mcp"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=300")
    );
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["resource"], "http://localhost:3040/mcp");
    assert_eq!(
        body["bearer_methods_supported"],
        serde_json::json!(["header"])
    );
    assert!(
        body["scopes_supported"]
            .as_array()
            .unwrap()
            .contains(&Value::from("view_project"))
    );
    assert!(
        !body["scopes_supported"]
            .as_array()
            .unwrap()
            .contains(&Value::from("admin"))
    );
}

#[tokio::test]
async fn authorization_server_document_defaults_to_redmine_issuer_at_the_suffixed_path() {
    let harness = support::http_harness(&oauth_env(&[])).await;

    let response = raw_client()
        .get(harness.url("/.well-known/oauth-authorization-server/mcp"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["issuer"], harness.redmine.uri());
    assert_eq!(
        body["authorization_endpoint"],
        format!("{}/oauth/authorize", harness.redmine.uri())
    );
    assert_eq!(
        body["response_types_supported"],
        serde_json::json!(["code"])
    );

    // The root path is not this mode's canonical location.
    let root = raw_client()
        .get(harness.url("/.well-known/oauth-authorization-server"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(root.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn self_discovery_mode_serves_the_as_document_at_the_root_and_404s_the_suffixed_path() {
    let harness =
        support::http_harness(&oauth_env(&[("REDMINE_OAUTH_DISCOVERY_AS", "self")])).await;

    let root = raw_client()
        .get(harness.url("/.well-known/oauth-authorization-server"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(root.status(), StatusCode::OK);
    let body: Value = root.json().await.expect("json body");
    assert_eq!(body["issuer"], "http://localhost:3040");
    // Authorize/token still go directly to Redmine.
    assert_eq!(
        body["authorization_endpoint"],
        format!("{}/oauth/authorize", harness.redmine.uri())
    );

    let suffixed = raw_client()
        .get(harness.url("/.well-known/oauth-authorization-server/mcp"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(suffixed.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn scopes_supported_reflects_read_only_and_is_identical_in_both_documents() {
    let harness = support::http_harness(&oauth_env(&[("REDMINE_MCP_READ_ONLY", "true")])).await;

    let prm: Value = raw_client()
        .get(harness.url("/.well-known/oauth-protected-resource/mcp"))
        .send()
        .await
        .expect("request should complete")
        .json()
        .await
        .expect("json body");
    let as_doc: Value = raw_client()
        .get(harness.url("/.well-known/oauth-authorization-server/mcp"))
        .send()
        .await
        .expect("request should complete")
        .json()
        .await
        .expect("json body");

    assert_eq!(prm["scopes_supported"], as_doc["scopes_supported"]);
    let scopes = prm["scopes_supported"].as_array().unwrap();
    assert!(scopes.iter().any(|s| s == "view_project"));
    assert!(!scopes.iter().any(|s| s == "edit_issues"));
}

#[tokio::test]
async fn redmine_mcp_scopes_narrows_the_advertised_set() {
    let harness = support::http_harness(&oauth_env(&[(
        "REDMINE_MCP_SCOPES",
        "view_project view_issues",
    )]))
    .await;

    let prm: Value = raw_client()
        .get(harness.url("/.well-known/oauth-protected-resource/mcp"))
        .send()
        .await
        .expect("request should complete")
        .json()
        .await
        .expect("json body");
    assert_eq!(
        prm["scopes_supported"],
        serde_json::json!(["view_project", "view_issues"])
    );
}

#[tokio::test]
async fn discovery_documents_leak_no_introspection_credential() {
    // Redmine's own URL is legitimately present (it is the AS issuer and
    // the target of `authorization_endpoint`/etc.) — what must never appear
    // is this server's introspection client id/secret, which the documents
    // have no reason to carry.
    let harness = support::http_harness(&oauth_env(&[])).await;
    for well_known in [
        "/.well-known/oauth-protected-resource/mcp",
        "/.well-known/oauth-authorization-server/mcp",
    ] {
        let text = raw_client()
            .get(harness.url(well_known))
            .send()
            .await
            .expect("request should complete")
            .text()
            .await
            .expect("text body");
        assert!(!text.contains(CLIENT_ID), "{text}");
        assert!(!text.contains(CLIENT_SECRET), "{text}");
    }
}

// --- POST /revoke (D4, D5) --------------------------------------------------

#[tokio::test]
async fn revoke_rejects_a_non_form_content_type_with_415() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    let response = raw_client()
        .post(harness.url("/revoke"))
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn revoke_rejects_an_oversized_body_with_413() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    let oversized = format!("token={}", "a".repeat(9 * 1024));
    let response = raw_client()
        .post(harness.url("/revoke"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(oversized)
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn revoke_requires_a_token_field() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    let response = raw_client()
        .post(harness.url("/revoke"))
        .header("content-type", "application/x-www-form-urlencoded")
        .basic_auth("some-client", Some("some-secret"))
        .body("token_type_hint=access_token")
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn revoke_requires_client_authentication() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    let response = raw_client()
        .post(harness.url("/revoke"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("token=some-token")
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn revoke_forwards_only_the_allowlisted_fields_and_drops_the_rest() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .and(basic_auth("user-client", "user-secret"))
        .and(body_string_contains("token=the-token"))
        .and(body_string_contains("token_type_hint=access_token"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&harness.redmine)
        .await;

    let response = raw_client()
        .post(harness.url("/revoke"))
        .header("content-type", "application/x-www-form-urlencoded")
        .basic_auth("user-client", Some("user-secret"))
        .body("token=the-token&token_type_hint=access_token&injected_field=should_be_dropped")
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::OK);
    // Dropping the harness verifies wiremock's `expect(1)` and the exact
    // body assertions above: an injected extra field is not sent upstream,
    // and neither is our own introspection client's credential.
}

#[tokio::test]
async fn revoke_accepts_client_credentials_as_form_fields_when_no_authorization_header_is_sent() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .and(basic_auth("form-client", "form-secret"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&harness.redmine)
        .await;

    let response = raw_client()
        .post(harness.url("/revoke"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("token=the-token&client_id=form-client&client_secret=form-secret")
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_revoked_token_stops_working_on_the_very_next_call_not_after_the_ttl() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    const TOKEN: &str = "the-users-access-token";
    mock_current_user(&harness.redmine, TOKEN).await;

    // `initialize` (O1) performs the one introspection this token ever needs
    // before revocation; `up_to_n_times(1)` exhausts this mock there, so the
    // *next* introspection — forced by `/revoke`'s cache purge (D5) — falls
    // through to the unconditional `active: false` mock below, simulating
    // Redmine having actually revoked the token. Same (default) priority
    // plus insertion order is what makes wiremock try this one first.
    Mock::given(method("POST"))
        .and(path("/oauth/introspect"))
        .and(basic_auth(CLIENT_ID, CLIENT_SECRET))
        .and(body_string_contains(format!("token={TOKEN}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "active": true, "sub": "5", "username": "alice"
        })))
        .up_to_n_times(1)
        .expect(1)
        .mount(&harness.redmine)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/introspect"))
        .and(body_string_contains(format!("token={TOKEN}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "active": false
        })))
        .mount(&harness.redmine)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .and(basic_auth(CLIENT_ID, CLIENT_SECRET))
        .and(body_string_contains(format!("token={TOKEN}")))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&harness.redmine)
        .await;

    let client = connect_with_token(&harness, TOKEN).await;
    client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await
        .expect("the token is active before revocation (served from the initialize-time cache)");

    let revoke_response = raw_client()
        .post(harness.url("/revoke"))
        .header("content-type", "application/x-www-form-urlencoded")
        .basic_auth(CLIENT_ID, Some(CLIENT_SECRET))
        .body(format!("token={TOKEN}"))
        .send()
        .await
        .expect("revoke request should complete");
    assert_eq!(revoke_response.status(), StatusCode::OK);

    client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await
        .expect_err("the very next call must fail, without waiting out the cache TTL");
    client.cancel().await.ok();
}

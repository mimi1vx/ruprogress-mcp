//! The full `oauth-proxy` authorization-code + PKCE round trip — register →
//! `/authorize` → (simulated) upstream redirect → `/auth/callback` →
//! `/token` → `tools/call` — over the real HTTP router against a wiremock
//! Redmine standing in for Doorkeeper, plus a `TRACE`-level redaction sweep
//! across the whole flow.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::items_after_statements
)]

mod support;

use std::collections::HashMap;

use base64::Engine as _;
use reqwest::StatusCode;
use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use sha2::{Digest as _, Sha256};
use wiremock::matchers::{basic_auth, body_string_contains, header, method, path};
use wiremock::{Mock, ResponseTemplate};

const UPSTREAM_CLIENT_ID: &str = "introspect-client";
const UPSTREAM_CLIENT_SECRET: &str = "introspect-secret";
const REDIRECT_URI: &str = "http://localhost/cb";
const CLIENT_STATE: &str = "client-state-123";

fn oauth_proxy_env(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut env = vec![
        ("REDMINE_AUTH_MODE", "oauth-proxy"),
        ("REDMINE_MCP_BASE_URL", "http://localhost:3040"),
        ("REDMINE_INTROSPECT_CLIENT_ID", UPSTREAM_CLIENT_ID),
        ("REDMINE_INTROSPECT_CLIENT_SECRET", UPSTREAM_CLIENT_SECRET),
    ];
    env.extend_from_slice(extra);
    env
}

fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("build a test HTTP client")
}

/// A valid RFC 7636 verifier/S256-challenge pair, standing in for what a
/// real MCP client would generate.
fn pkce_pair() -> (String, String) {
    let verifier = "test-client-code-verifier-0123456789-abcdefghi".to_string();
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());
    (verifier, challenge)
}

fn query_pairs(url: &url::Url) -> HashMap<String, String> {
    url.query_pairs().into_owned().collect()
}

/// Appends `pairs` to `base` as a query string. Workspace `reqwest` builds
/// without the `query` cargo feature (see `Cargo.toml`), so this replaces
/// `RequestBuilder::query` for these tests.
fn url_with_query(base: &str, pairs: &[(&str, &str)]) -> String {
    let mut url = url::Url::parse(base).expect("valid base url");
    {
        let mut serializer = url.query_pairs_mut();
        for (key, value) in pairs {
            serializer.append_pair(key, value);
        }
    }
    url.into()
}

async fn register_client(harness: &support::HttpHarness) -> String {
    let response = reqwest::Client::new()
        .post(harness.url("/register"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({ "redirect_uris": [REDIRECT_URI] }))
        .send()
        .await
        .expect("register request should complete");
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: serde_json::Value = response.json().await.expect("json body");
    body["client_id"]
        .as_str()
        .expect("client_id present")
        .to_string()
}

/// Drives `GET /authorize` and returns `(transaction_state, upstream_challenge)`
/// read off the `Location` this server issues toward Redmine's own
/// authorize endpoint.
async fn drive_authorize(
    harness: &support::HttpHarness,
    client_id: &str,
    code_challenge: &str,
) -> (String, String) {
    let response = no_redirect_client()
        .get(url_with_query(
            &harness.url("/authorize"),
            &[
                ("response_type", "code"),
                ("client_id", client_id),
                ("redirect_uri", REDIRECT_URI),
                ("code_challenge", code_challenge),
                ("code_challenge_method", "S256"),
                ("state", CLIENT_STATE),
                ("scope", "view_project view_issues"),
            ],
        ))
        .send()
        .await
        .expect("authorize request should complete");
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .expect("Location header present")
        .to_str()
        .expect("valid header value")
        .to_string();
    let url = url::Url::parse(&location).expect("valid upstream authorize url");
    assert_eq!(url.path(), "/oauth/authorize");
    let pairs = query_pairs(&url);
    assert_eq!(
        pairs.get("client_id").map(String::as_str),
        Some(UPSTREAM_CLIENT_ID)
    );
    assert_eq!(
        pairs.get("redirect_uri").map(String::as_str),
        Some("http://localhost:3040/auth/callback")
    );
    assert_eq!(
        pairs.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    let upstream_challenge = pairs
        .get("code_challenge")
        .expect("code_challenge present")
        .clone();
    assert_ne!(
        upstream_challenge, code_challenge,
        "upstream PKCE must never be the client's own (F3)"
    );
    let transaction_state = pairs.get("state").expect("state present").clone();
    (transaction_state, upstream_challenge)
}

async fn mock_upstream_token_exchange(redmine: &wiremock::MockServer, access_token: &str) {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(basic_auth(UPSTREAM_CLIENT_ID, UPSTREAM_CLIENT_SECRET))
        .and(body_string_contains("grant_type=authorization_code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": access_token,
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "view_project view_issues",
        })))
        .mount(redmine)
        .await;
}

async fn mock_introspect_active(redmine: &wiremock::MockServer, access_token: &str) {
    Mock::given(method("POST"))
        .and(path("/oauth/introspect"))
        .and(basic_auth(UPSTREAM_CLIENT_ID, UPSTREAM_CLIENT_SECRET))
        .and(body_string_contains(format!("token={access_token}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "active": true,
            "sub": "5",
            "username": "alice",
            "scope": "view_project view_issues",
        })))
        .mount(redmine)
        .await;
}

async fn mock_current_user(redmine: &wiremock::MockServer, access_token: &str) {
    Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .and(header("authorization", format!("Bearer {access_token}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "user": {
                "id": 5,
                "login": "alice",
                "firstname": "Alice",
                "lastname": "Example",
                "mail": "alice@example.com",
                "created_on": "2024-01-01T00:00:00Z",
            }
        })))
        .mount(redmine)
        .await;
}

/// Drives `GET /auth/callback` with a fake upstream `code`, returning the
/// `code` this server minted for the client.
async fn drive_callback(harness: &support::HttpHarness, transaction_state: &str) -> String {
    let response = no_redirect_client()
        .get(url_with_query(
            &harness.url("/auth/callback"),
            &[
                ("code", "fake-upstream-authorization-code"),
                ("state", transaction_state),
            ],
        ))
        .send()
        .await
        .expect("callback request should complete");
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .expect("Location header present")
        .to_str()
        .expect("valid header value")
        .to_string();
    let url = url::Url::parse(&location).expect("valid client redirect url");
    assert_eq!(
        format!(
            "{}://{}{}",
            url.scheme(),
            url.host_str().unwrap(),
            url.path()
        ),
        REDIRECT_URI
    );
    let pairs = query_pairs(&url);
    assert_eq!(pairs.get("state").map(String::as_str), Some(CLIENT_STATE));
    assert!(pairs.contains_key("iss"));
    pairs.get("code").expect("code present").clone()
}

/// Redeems `code` at `POST /token`, returning the raw response body.
async fn drive_token(
    harness: &support::HttpHarness,
    code: &str,
    client_id: &str,
    code_verifier: &str,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(harness.url("/token"))
        .header("content-type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT_URI),
            ("client_id", client_id),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .expect("token request should complete")
}

/// The full happy path, driven exactly as a real client would: register →
/// authorize → (simulated) upstream callback → token → `tools/call`.
/// Returns the minted proxy access token, for tests that need to keep
/// driving the flow (replay, refresh-shaped grants, etc.).
async fn full_round_trip(harness: &support::HttpHarness, upstream_access_token: &str) -> String {
    mock_upstream_token_exchange(&harness.redmine, upstream_access_token).await;
    mock_introspect_active(&harness.redmine, upstream_access_token).await;
    mock_current_user(&harness.redmine, upstream_access_token).await;

    let client_id = register_client(harness).await;
    let (verifier, challenge) = pkce_pair();
    let (transaction_state, _upstream_challenge) =
        drive_authorize(harness, &client_id, &challenge).await;
    let code = drive_callback(harness, &transaction_state).await;

    let response = drive_token(harness, &code, &client_id, &verifier).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .expect("Cache-Control present"),
        "no-store"
    );
    let body: serde_json::Value = response.json().await.expect("json body");
    let proxy_access_token = body["access_token"]
        .as_str()
        .expect("access_token present")
        .to_string();
    assert!(proxy_access_token.starts_with("rup_at_"));
    assert_eq!(body["token_type"], "Bearer");
    assert!(!body.to_string().contains(upstream_access_token));

    proxy_access_token
}

#[tokio::test]
async fn full_round_trip_reaches_redmine_with_the_upstream_token_never_a_proxy_token() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const UPSTREAM_ACCESS_TOKEN: &str = "upstream-redmine-access-token-xyz";

    let proxy_access_token = full_round_trip(&harness, UPSTREAM_ACCESS_TOKEN).await;

    let config = StreamableHttpClientTransportConfig::with_uri(harness.mcp_url())
        .auth_header(proxy_access_token.clone());
    let transport = StreamableHttpClientTransport::from_config(config);
    let mcp_client =
        ().serve(transport)
            .await
            .expect("client with a valid proxy token should connect");
    let result = mcp_client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await
        .expect("tool call should succeed");
    mcp_client.cancel().await.ok();
    assert_ne!(result.is_error, Some(true));
}

#[tokio::test]
async fn the_upstream_token_presented_directly_to_mcp_is_401() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const UPSTREAM_ACCESS_TOKEN: &str = "upstream-redmine-access-token-direct";
    mock_introspect_active(&harness.redmine, UPSTREAM_ACCESS_TOKEN).await;

    let response = reqwest::Client::new()
        .post(harness.mcp_url())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("authorization", format!("Bearer {UPSTREAM_ACCESS_TOKEN}"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "oauth-proxy-test", "version": "0" }
            }
        }))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn code_replay_is_invalid_grant_and_revokes_the_session_it_created() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const UPSTREAM_ACCESS_TOKEN: &str = "upstream-redmine-access-token-replay";
    mock_upstream_token_exchange(&harness.redmine, UPSTREAM_ACCESS_TOKEN).await;
    mock_introspect_active(&harness.redmine, UPSTREAM_ACCESS_TOKEN).await;

    let client_id = register_client(&harness).await;
    let (verifier, challenge) = pkce_pair();
    let (transaction_state, _) = drive_authorize(&harness, &client_id, &challenge).await;
    let code = drive_callback(&harness, &transaction_state).await;

    let first = drive_token(&harness, &code, &client_id, &verifier).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first_body: serde_json::Value = first.json().await.expect("json body");
    let minted_token = first_body["access_token"]
        .as_str()
        .expect("token")
        .to_string();

    let second = drive_token(&harness, &code, &client_id, &verifier).await;
    assert_eq!(second.status(), StatusCode::BAD_REQUEST);
    let second_body: serde_json::Value = second.json().await.expect("json body");
    assert_eq!(second_body["error"], "invalid_grant");

    // F8: the first exchange's token must stop working immediately.
    let config =
        StreamableHttpClientTransportConfig::with_uri(harness.mcp_url()).auth_header(minted_token);
    let transport = StreamableHttpClientTransport::from_config(config);
    let connect = ().serve(transport).await;
    assert!(
        connect.is_err(),
        "a revoked (replayed) proxy token must not authenticate"
    );
}

#[tokio::test]
async fn wrong_code_verifier_foreign_client_id_and_mismatched_redirect_uri_are_all_invalid_grant() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const UPSTREAM_ACCESS_TOKEN: &str = "upstream-redmine-access-token-mismatch";
    mock_upstream_token_exchange(&harness.redmine, UPSTREAM_ACCESS_TOKEN).await;

    let client_id = register_client(&harness).await;
    let other_client_id = register_client(&harness).await;
    let (verifier, challenge) = pkce_pair();
    let (transaction_state, _) = drive_authorize(&harness, &client_id, &challenge).await;
    let code = drive_callback(&harness, &transaction_state).await;

    let wrong_verifier = drive_token(&harness, &code, &client_id, "not-the-right-verifier").await;
    assert_eq!(wrong_verifier.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = wrong_verifier.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_grant");

    let foreign_client = drive_token(&harness, &code, &other_client_id, &verifier).await;
    assert_eq!(foreign_client.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = foreign_client.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_grant");

    let response = reqwest::Client::new()
        .post(harness.url("/token"))
        .header("content-type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", "http://localhost/a-different-path"),
            ("client_id", client_id.as_str()),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_grant");
}

#[tokio::test]
async fn every_authorize_error_before_redirect_uri_validation_has_no_location_header() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    let client_id = register_client(&harness).await;

    // Unknown client_id.
    let response = no_redirect_client()
        .get(url_with_query(
            &harness.url("/authorize"),
            &[
                ("response_type", "code"),
                ("client_id", "not-a-registered-client"),
                ("redirect_uri", REDIRECT_URI),
                ("code_challenge", "x"),
                ("code_challenge_method", "S256"),
            ],
        ))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.headers().get(reqwest::header::LOCATION).is_none());

    // A redirect_uri never registered for this client.
    let response = no_redirect_client()
        .get(url_with_query(
            &harness.url("/authorize"),
            &[
                ("response_type", "code"),
                ("client_id", client_id.as_str()),
                ("redirect_uri", "http://localhost/never-registered"),
                ("code_challenge", "x"),
                ("code_challenge_method", "S256"),
            ],
        ))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.headers().get(reqwest::header::LOCATION).is_none());

    // Missing client_id entirely.
    let response = no_redirect_client()
        .get(harness.url("/authorize"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.headers().get(reqwest::header::LOCATION).is_none());
}

#[tokio::test]
async fn phase_b_failures_redirect_with_error_state_and_iss() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    let client_id = register_client(&harness).await;

    let response = no_redirect_client()
        .get(url_with_query(
            &harness.url("/authorize"),
            &[
                ("response_type", "token"),
                ("client_id", client_id.as_str()),
                ("redirect_uri", REDIRECT_URI),
                ("code_challenge", "x"),
                ("code_challenge_method", "S256"),
                ("state", CLIENT_STATE),
            ],
        ))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .expect("Location header present")
        .to_str()
        .expect("valid header value")
        .to_string();
    let url = url::Url::parse(&location).expect("valid redirect url");
    let pairs = query_pairs(&url);
    assert_eq!(
        pairs.get("error").map(String::as_str),
        Some("unsupported_response_type")
    );
    assert_eq!(pairs.get("state").map(String::as_str), Some(CLIENT_STATE));
    assert!(pairs.contains_key("iss"));
}

#[tokio::test]
async fn an_unadvertised_scope_is_invalid_scope() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    let client_id = register_client(&harness).await;
    let (_, challenge) = pkce_pair();

    let response = no_redirect_client()
        .get(url_with_query(
            &harness.url("/authorize"),
            &[
                ("response_type", "code"),
                ("client_id", client_id.as_str()),
                ("redirect_uri", REDIRECT_URI),
                ("code_challenge", challenge.as_str()),
                ("code_challenge_method", "S256"),
                ("state", CLIENT_STATE),
                ("scope", "this_scope_does_not_exist"),
            ],
        ))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .expect("Location header present")
        .to_str()
        .expect("valid header value")
        .to_string();
    let url = url::Url::parse(&location).expect("valid redirect url");
    let pairs = query_pairs(&url);
    assert_eq!(
        pairs.get("error").map(String::as_str),
        Some("invalid_scope")
    );
}

// --- redaction (risk 5) ------------------------------------------------------

/// No proxy token, upstream token, upstream client secret, `code`, or
/// `code_verifier` may appear in captured `TRACE` output across the whole
/// flow (risk 5).
#[tokio::test(flavor = "current_thread")]
async fn no_secret_appears_in_captured_trace_logs_across_the_whole_flow() {
    let capture = support::capture("trace").await;

    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const UPSTREAM_ACCESS_TOKEN: &str = "super-secret-upstream-access-token-0123456789";
    const UPSTREAM_CODE: &str = "super-secret-fake-upstream-code-abcdefghijk";

    mock_upstream_token_exchange(&harness.redmine, UPSTREAM_ACCESS_TOKEN).await;
    mock_introspect_active(&harness.redmine, UPSTREAM_ACCESS_TOKEN).await;
    mock_current_user(&harness.redmine, UPSTREAM_ACCESS_TOKEN).await;

    let client_id = register_client(&harness).await;
    let (verifier, challenge) = pkce_pair();
    let (transaction_state, _) = drive_authorize(&harness, &client_id, &challenge).await;

    let response = no_redirect_client()
        .get(url_with_query(
            &harness.url("/auth/callback"),
            &[("code", UPSTREAM_CODE), ("state", &transaction_state)],
        ))
        .send()
        .await
        .expect("callback request should complete");
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .expect("Location header present")
        .to_str()
        .expect("valid header value")
        .to_string();
    let code = query_pairs(&url::Url::parse(&location).expect("valid url"))
        .get("code")
        .expect("code present")
        .clone();

    let token_response = drive_token(&harness, &code, &client_id, &verifier).await;
    let body: serde_json::Value = token_response.json().await.expect("json body");
    let proxy_access_token = body["access_token"].as_str().expect("token").to_string();

    let config = StreamableHttpClientTransportConfig::with_uri(harness.mcp_url())
        .auth_header(proxy_access_token.clone());
    let transport = StreamableHttpClientTransport::from_config(config);
    let mcp_client = ().serve(transport).await.expect("client should connect");
    mcp_client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await
        .expect("tool call should succeed");
    mcp_client.cancel().await.ok();

    capture.assert_no_secrets(&[
        UPSTREAM_ACCESS_TOKEN,
        UPSTREAM_CODE,
        UPSTREAM_CLIENT_SECRET,
        proxy_access_token.as_str(),
        verifier.as_str(),
        code.as_str(),
    ]);
}

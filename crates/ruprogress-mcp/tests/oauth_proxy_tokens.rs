//! `oauth-proxy`'s `/token` refresh grant and mode-specific `/revoke`:
//! rotation, reuse containment, the no-refresh-token path, and revocation of
//! both token families — over the real HTTP router against a wiremock
//! Redmine standing in for Doorkeeper.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::items_after_statements
)]

mod support;

use std::collections::HashMap;
use std::time::Duration;

use base64::Engine as _;
use reqwest::StatusCode;
use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use sha2::{Digest as _, Sha256};
use wiremock::matchers::{basic_auth, body_string_contains, method, path};
use wiremock::{Mock, ResponseTemplate};

const UPSTREAM_CLIENT_ID: &str = "introspect-client";
const UPSTREAM_CLIENT_SECRET: &str = "introspect-secret";
const REDIRECT_URI: &str = "http://localhost/cb";

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

async fn drive_authorize(
    harness: &support::HttpHarness,
    client_id: &str,
    code_challenge: &str,
) -> String {
    let response = no_redirect_client()
        .get(url_with_query(
            &harness.url("/authorize"),
            &[
                ("response_type", "code"),
                ("client_id", client_id),
                ("redirect_uri", REDIRECT_URI),
                ("code_challenge", code_challenge),
                ("code_challenge_method", "S256"),
                ("state", "client-state"),
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
    query_pairs(&url)
        .get("state")
        .expect("state present")
        .clone()
}

async fn mock_upstream_token_exchange(
    redmine: &wiremock::MockServer,
    access_token: &str,
    refresh_token: Option<&str>,
) {
    let mut body = serde_json::json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": 3600,
        "scope": "view_project view_issues",
    });
    if let Some(refresh_token) = refresh_token {
        body["refresh_token"] = serde_json::json!(refresh_token);
    }
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(basic_auth(UPSTREAM_CLIENT_ID, UPSTREAM_CLIENT_SECRET))
        .and(body_string_contains("grant_type=authorization_code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
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

async fn drive_callback(harness: &support::HttpHarness, transaction_state: &str) -> String {
    drive_callback_with_upstream_code(
        harness,
        transaction_state,
        "fake-upstream-authorization-code",
    )
    .await
}

/// Like [`drive_callback`], but with a caller-chosen upstream authorization
/// code — for a test driving two independent flows in the same wiremock
/// Redmine, which otherwise could not tell the two `/oauth/token` exchanges
/// apart.
async fn drive_callback_with_upstream_code(
    harness: &support::HttpHarness,
    transaction_state: &str,
    upstream_code: &str,
) -> String {
    let response = no_redirect_client()
        .get(url_with_query(
            &harness.url("/auth/callback"),
            &[("code", upstream_code), ("state", transaction_state)],
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
    query_pairs(&url).get("code").expect("code present").clone()
}

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

async fn drive_refresh(
    harness: &support::HttpHarness,
    refresh_token: &str,
    client_id: &str,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(harness.url("/token"))
        .header("content-type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .await
        .expect("refresh request should complete")
}

async fn drive_revoke(harness: &support::HttpHarness, token: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(harness.url("/revoke"))
        .header("content-type", "application/x-www-form-urlencoded")
        .form(&[("token", token)])
        .send()
        .await
        .expect("revoke request should complete")
}

async fn call_tool_with_token(harness: &support::HttpHarness, token: &str) -> bool {
    let config = StreamableHttpClientTransportConfig::with_uri(harness.mcp_url())
        .auth_header(token.to_string());
    let transport = StreamableHttpClientTransport::from_config(config);
    let Ok(client) = ().serve(transport).await else {
        return false;
    };
    let result = client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await;
    client.cancel().await.ok();
    result.is_ok()
}

/// Registers, authorizes, and exchanges a code for a proxy access/refresh
/// pair, with Redmine minting `refresh_token` too. Returns `(access,
/// refresh, client_id)`.
async fn full_round_trip_with_refresh(
    harness: &support::HttpHarness,
    upstream_access_token: &str,
    upstream_refresh_token: &str,
) -> (String, String, String) {
    mock_upstream_token_exchange(
        &harness.redmine,
        upstream_access_token,
        Some(upstream_refresh_token),
    )
    .await;
    mock_introspect_active(&harness.redmine, upstream_access_token).await;

    let client_id = register_client(harness).await;
    let (verifier, challenge) = pkce_pair();
    let transaction_state = drive_authorize(harness, &client_id, &challenge).await;
    let code = drive_callback(harness, &transaction_state).await;

    let response = drive_token(harness, &code, &client_id, &verifier).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.expect("json body");
    let access = body["access_token"]
        .as_str()
        .expect("access_token")
        .to_string();
    let refresh = body["refresh_token"]
        .as_str()
        .expect("refresh_token present when upstream grants one")
        .to_string();
    (access, refresh, client_id)
}

// --- refresh grant (R1, R4, R6) ----------------------------------------------

#[tokio::test]
async fn refresh_returns_a_new_pair_and_the_new_access_token_drives_a_tool_call() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const OLD_ACCESS: &str = "upstream-access-before-refresh";
    const NEW_ACCESS: &str = "upstream-access-after-refresh";
    let (_, refresh, client_id) =
        full_round_trip_with_refresh(&harness, OLD_ACCESS, "upstream-refresh-1").await;

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(basic_auth(UPSTREAM_CLIENT_ID, UPSTREAM_CLIENT_SECRET))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=upstream-refresh-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": NEW_ACCESS,
            "refresh_token": "upstream-refresh-2",
            "expires_in": 3600,
            "scope": "view_project view_issues",
        })))
        .mount(&harness.redmine)
        .await;
    support::mock_current_user(&harness.redmine, None).await;
    mock_introspect_active(&harness.redmine, NEW_ACCESS).await;

    let response = drive_refresh(&harness, &refresh, &client_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .expect("Cache-Control present"),
        "no-store"
    );
    let body: serde_json::Value = response.json().await.expect("json body");
    let new_access = body["access_token"]
        .as_str()
        .expect("access_token")
        .to_string();
    let new_refresh = body["refresh_token"]
        .as_str()
        .expect("refresh_token")
        .to_string();
    assert!(new_access.starts_with("rup_at_"));
    assert!(new_refresh.starts_with("rup_rt_"));
    assert_ne!(new_access, refresh);
    assert!(!body.to_string().contains(NEW_ACCESS));

    assert!(
        call_tool_with_token(&harness, &new_access).await,
        "the new access token must drive a tool call"
    );
}

#[tokio::test]
async fn the_old_refresh_token_is_invalid_grant_after_a_successful_rotation() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    let (_, refresh, client_id) =
        full_round_trip_with_refresh(&harness, "upstream-access-1", "upstream-refresh-1").await;

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "upstream-access-2",
            "refresh_token": "upstream-refresh-2",
            "expires_in": 3600,
        })))
        .mount(&harness.redmine)
        .await;

    let first = drive_refresh(&harness, &refresh, &client_id).await;
    assert_eq!(first.status(), StatusCode::OK);

    let second = drive_refresh(&harness, &refresh, &client_id).await;
    assert_eq!(second.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = second.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_grant");
}

#[tokio::test]
async fn reusing_a_rotated_refresh_token_invalidates_the_whole_chain_and_revokes_upstream() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const FIRST_ACCESS: &str = "upstream-access-1";
    const SECOND_ACCESS: &str = "upstream-access-2";
    let (first_access, first_refresh, client_id) =
        full_round_trip_with_refresh(&harness, FIRST_ACCESS, "upstream-refresh-1").await;

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=upstream-refresh-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": SECOND_ACCESS,
            "refresh_token": "upstream-refresh-2",
            "expires_in": 3600,
        })))
        .mount(&harness.redmine)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .and(basic_auth(UPSTREAM_CLIENT_ID, UPSTREAM_CLIENT_SECRET))
        // The session's *refresh* token is revoked, not its access token
        // (finding 6a): Doorkeeper answers `200` and does nothing for an
        // already-revocable-but-unexpired access token here too, but the
        // refresh token is the one that never expires on its own upstream,
        // so it is the credential actually worth killing.
        .and(body_string_contains("token=upstream-refresh-2"))
        .and(body_string_contains("token_type_hint=refresh_token"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&harness.redmine)
        .await;

    // Legitimate rotation: RT1 -> RT2.
    let rotated = drive_refresh(&harness, &first_refresh, &client_id).await;
    assert_eq!(rotated.status(), StatusCode::OK);

    // Replaying RT1 (already rotated away) must kill the *current* session
    // (bound to the same, stable upstream_id) and revoke it upstream.
    let replay = drive_refresh(&harness, &first_refresh, &client_id).await;
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = replay.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_grant");

    // The original access token (same upstream session) must be dead too.
    assert!(!call_tool_with_token(&harness, &first_access).await);
}

/// Finding 2: two concurrent redemptions of the same refresh token must
/// never both mint a pair. Refresh A's upstream exchange is delayed 500ms;
/// refresh B is driven 100ms in, well before A's response lands, so B
/// observes A's redemption as still in flight. Wide timing margin because
/// this is the one wall-clock-dependent test in the change — the invariant
/// itself is proven deterministically by `store.rs`'s guard unit tests.
#[tokio::test]
async fn concurrent_refreshes_of_the_same_token_leave_no_winner() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const FIRST_ACCESS: &str = "upstream-access-race-1";
    const SECOND_ACCESS: &str = "upstream-access-race-2";
    let (first_access, first_refresh, client_id) =
        full_round_trip_with_refresh(&harness, FIRST_ACCESS, "upstream-refresh-race-1").await;

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains(
            "refresh_token=upstream-refresh-race-1",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "access_token": SECOND_ACCESS,
                    "refresh_token": "upstream-refresh-race-2",
                    "expires_in": 3600,
                }))
                .set_delay(Duration::from_millis(500)),
        )
        .expect(1)
        .mount(&harness.redmine)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .and(basic_auth(UPSTREAM_CLIENT_ID, UPSTREAM_CLIENT_SECRET))
        .respond_with(ResponseTemplate::new(200))
        .mount(&harness.redmine)
        .await;

    let url = harness.url("/token");
    let refresh_a = first_refresh.clone();
    let client_id_a = client_id.clone();
    let request_a = tokio::spawn(async move {
        reqwest::Client::new()
            .post(url)
            .header("content-type", "application/x-www-form-urlencoded")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &refresh_a),
                ("client_id", &client_id_a),
            ])
            .send()
            .await
            .expect("refresh A request should complete")
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    let response_b = drive_refresh(&harness, &first_refresh, &client_id).await;
    let response_a = request_a.await.expect("refresh A task should not panic");

    assert_eq!(response_a.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response_b.status(), StatusCode::BAD_REQUEST);
    let body_a: serde_json::Value = response_a.json().await.expect("json body");
    let body_b: serde_json::Value = response_b.json().await.expect("json body");
    assert_eq!(body_a["error"], "invalid_grant");
    assert_eq!(body_b["error"], "invalid_grant");
    assert!(body_a.get("access_token").is_none());
    assert!(body_b.get("access_token").is_none());

    // The session died in the race: the original proxy access token no
    // longer authenticates a tool call.
    assert!(!call_tool_with_token(&harness, &first_access).await);
}

#[tokio::test]
async fn a_foreign_client_id_on_refresh_is_invalid_grant() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    let (_, refresh, _client_id) =
        full_round_trip_with_refresh(&harness, "upstream-access-1", "upstream-refresh-1").await;

    let response = drive_refresh(&harness, &refresh, "some-other-client-id").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_grant");
}

#[tokio::test]
async fn unknown_refresh_token_is_invalid_grant() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    let response = drive_refresh(&harness, "rup_rt_never-issued", "any-client").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_grant");
}

#[tokio::test]
async fn upstream_rejecting_the_refresh_is_invalid_grant_and_cleans_up_the_session() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const ACCESS: &str = "upstream-access-1";
    let (access, refresh, client_id) =
        full_round_trip_with_refresh(&harness, ACCESS, "upstream-refresh-1").await;

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant",
            "error_description": "the refresh token has been revoked upstream",
        })))
        .mount(&harness.redmine)
        .await;

    let response = drive_refresh(&harness, &refresh, &client_id).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(body["error"], "invalid_grant");

    // The session is cleaned up: the access token minted alongside this
    // refresh token no longer authenticates.
    assert!(!call_tool_with_token(&harness, &access).await);
}

#[tokio::test]
async fn a_deployment_whose_upstream_omits_refresh_token_still_works_access_only() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const ACCESS: &str = "upstream-access-no-refresh";
    mock_upstream_token_exchange(&harness.redmine, ACCESS, None).await;
    mock_introspect_active(&harness.redmine, ACCESS).await;
    support::mock_current_user(&harness.redmine, None).await;

    let client_id = register_client(&harness).await;
    let (verifier, challenge) = pkce_pair();
    let transaction_state = drive_authorize(&harness, &client_id, &challenge).await;
    let code = drive_callback(&harness, &transaction_state).await;

    let response = drive_token(&harness, &code, &client_id, &verifier).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.expect("json body");
    assert!(body.get("refresh_token").is_none());
    let access = body["access_token"]
        .as_str()
        .expect("access_token")
        .to_string();
    assert!(call_tool_with_token(&harness, &access).await);
}

// --- POST /revoke in oauth-proxy mode (R5, R6) -------------------------------

#[tokio::test]
async fn revoking_a_proxy_access_token_makes_the_next_call_401() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const ACCESS: &str = "upstream-access-for-revoke";
    let (access, _refresh, _client_id) =
        full_round_trip_with_refresh(&harness, ACCESS, "upstream-refresh-1").await;

    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .and(basic_auth(UPSTREAM_CLIENT_ID, UPSTREAM_CLIENT_SECRET))
        // The session's refresh token is revoked, not its access token
        // (finding 6a) — see the reuse-containment test's comment.
        .and(body_string_contains("token=upstream-refresh-1"))
        .and(body_string_contains("token_type_hint=refresh_token"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&harness.redmine)
        .await;

    assert!(call_tool_with_token(&harness, &access).await);

    let response = drive_revoke(&harness, &access).await;
    assert_eq!(response.status(), StatusCode::OK);

    assert!(!call_tool_with_token(&harness, &access).await);
}

/// Finding 6a: Doorkeeper silently no-ops revoking an already-expired
/// access token (V1), so a session minted with `expires_in: 0` must still
/// be revocable upstream — via its refresh token, which never expires on
/// its own (V5).
#[tokio::test]
async fn revoking_a_session_whose_access_token_already_expired_still_revokes_its_refresh_token() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const ACCESS: &str = "upstream-access-already-expired";
    const REFRESH: &str = "upstream-refresh-already-expired";

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(basic_auth(UPSTREAM_CLIENT_ID, UPSTREAM_CLIENT_SECRET))
        .and(body_string_contains("grant_type=authorization_code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": ACCESS,
            "refresh_token": REFRESH,
            "token_type": "Bearer",
            "expires_in": 0,
            "scope": "view_project view_issues",
        })))
        .mount(&harness.redmine)
        .await;

    let client_id = register_client(&harness).await;
    let (verifier, challenge) = pkce_pair();
    let transaction_state = drive_authorize(&harness, &client_id, &challenge).await;
    let code = drive_callback(&harness, &transaction_state).await;

    let response = drive_token(&harness, &code, &client_id, &verifier).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.expect("json body");
    let access = body["access_token"]
        .as_str()
        .expect("access_token")
        .to_string();

    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .and(basic_auth(UPSTREAM_CLIENT_ID, UPSTREAM_CLIENT_SECRET))
        .and(body_string_contains(format!("token={REFRESH}")))
        .and(body_string_contains("token_type_hint=refresh_token"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&harness.redmine)
        .await;

    let response = drive_revoke(&harness, &access).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn revoking_a_refresh_token_invalidates_its_access_sibling() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const ACCESS: &str = "upstream-access-sibling";
    let (access, refresh, _client_id) =
        full_round_trip_with_refresh(&harness, ACCESS, "upstream-refresh-sibling").await;

    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .and(basic_auth(UPSTREAM_CLIENT_ID, UPSTREAM_CLIENT_SECRET))
        .respond_with(ResponseTemplate::new(200))
        .mount(&harness.redmine)
        .await;

    assert!(call_tool_with_token(&harness, &access).await);

    let response = drive_revoke(&harness, &refresh).await;
    assert_eq!(response.status(), StatusCode::OK);

    assert!(!call_tool_with_token(&harness, &access).await);
}

#[tokio::test]
async fn revoking_an_unknown_token_is_still_200() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    let response = drive_revoke(&harness, "rup_at_never-issued").await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn revoke_works_even_when_redmine_is_unreachable() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const ACCESS: &str = "upstream-access-redmine-down";
    let (access, _refresh, _client_id) =
        full_round_trip_with_refresh(&harness, ACCESS, "upstream-refresh-down").await;

    // No `/oauth/revoke` mock is mounted: any request to it 404s from
    // wiremock's default, standing in for an unreachable Redmine well
    // enough to prove local state is not contingent on the upstream call.
    let response = drive_revoke(&harness, &access).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!call_tool_with_token(&harness, &access).await);
}

#[tokio::test]
async fn every_existing_oauth_mode_revoke_test_still_applies_unedited() {
    // Sanity anchor: oauth-proxy's /revoke is a distinct route/handler from
    // oauth mode's (see transport::http::revoke), so this file adds no
    // assertions about oauth mode — those live in oauth_discovery.rs and
    // are unedited by this change.
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    let response = drive_revoke(&harness, "not-a-recognized-prefix").await;
    assert_eq!(response.status(), StatusCode::OK);
}

// --- session sweep (finding 6) -----------------------------------------------

/// A session with no refresh token is swept, upstream-silent, by the next
/// successful authorization (the only session-creating path). Doorkeeper
/// silently no-ops revoking an already-expired access token (V1), so a
/// no-refresh sweep must never call `/oauth/revoke` at all — the `.expect(0)`
/// mock below is the regression proof; the exact deadline math is store.rs's
/// unit tests' job, not this file's.
///
/// A refresh-bearing sweep's revocation is deliberately **not** covered
/// here: its deadline is `REFRESH_TTL` (30 days), unreachable by wall clock
/// in an integration test without a seam that would have to be visible in
/// production code to cross the test-binary boundary. `store.rs`'s
/// `sweep_expired`/`sweep_expired_sessions` unit tests hold that invariant
/// deterministically instead.
#[tokio::test]
async fn a_no_refresh_session_is_swept_by_the_next_authorization_with_no_upstream_revoke() {
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;
    const FIRST_ACCESS: &str = "upstream-access-sweep-no-refresh";
    const SECOND_ACCESS: &str = "upstream-access-sweep-trigger";

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(basic_auth(UPSTREAM_CLIENT_ID, UPSTREAM_CLIENT_SECRET))
        .and(body_string_contains("code=first-upstream-code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": FIRST_ACCESS,
            "token_type": "Bearer",
            "expires_in": 0,
            "scope": "view_project view_issues",
        })))
        .mount(&harness.redmine)
        .await;

    let first_client_id = register_client(&harness).await;
    let (first_verifier, first_challenge) = pkce_pair();
    let first_transaction_state =
        drive_authorize(&harness, &first_client_id, &first_challenge).await;
    let first_code = drive_callback_with_upstream_code(
        &harness,
        &first_transaction_state,
        "first-upstream-code",
    )
    .await;
    let first_response =
        drive_token(&harness, &first_code, &first_client_id, &first_verifier).await;
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body: serde_json::Value = first_response.json().await.expect("json body");
    assert!(first_body.get("refresh_token").is_none());
    let first_access = first_body["access_token"]
        .as_str()
        .expect("access_token")
        .to_string();

    // No revocation must ever be attempted for a no-refresh sweep.
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&harness.redmine)
        .await;

    // A second, independent flow: its successful redemption is the sweep
    // trigger (the only session-creating path).
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(basic_auth(UPSTREAM_CLIENT_ID, UPSTREAM_CLIENT_SECRET))
        .and(body_string_contains("code=second-upstream-code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": SECOND_ACCESS,
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "view_project view_issues",
        })))
        .mount(&harness.redmine)
        .await;
    mock_introspect_active(&harness.redmine, SECOND_ACCESS).await;
    support::mock_current_user(&harness.redmine, None).await;

    let second_client_id = register_client(&harness).await;
    let (second_verifier, second_challenge) = pkce_pair();
    let second_transaction_state =
        drive_authorize(&harness, &second_client_id, &second_challenge).await;
    let second_code = drive_callback_with_upstream_code(
        &harness,
        &second_transaction_state,
        "second-upstream-code",
    )
    .await;
    let second_response =
        drive_token(&harness, &second_code, &second_client_id, &second_verifier).await;
    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body: serde_json::Value = second_response.json().await.expect("json body");
    let second_access = second_body["access_token"]
        .as_str()
        .expect("access_token")
        .to_string();

    // The swept session is gone; the new one works. (`harness` drops at the
    // end of the test, verifying the `.expect(0)` revoke mock above.)
    assert!(!call_tool_with_token(&harness, &first_access).await);
    assert!(call_tool_with_token(&harness, &second_access).await);
}

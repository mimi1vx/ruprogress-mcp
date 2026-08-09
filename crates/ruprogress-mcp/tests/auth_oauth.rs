//! End-to-end `AuthMode::OAuth`: bearer extraction, RFC 7662 introspection,
//! the `401`/`503` challenge, and the token cache — over the real HTTP
//! router, against a wiremock Redmine that also stands in for Doorkeeper.
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
use wiremock::matchers::{basic_auth, body_string_contains, header, method, path};
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

async fn mock_introspect(
    redmine: &wiremock::MockServer,
    token: &str,
    body: serde_json::Value,
    times: Option<u64>,
) {
    let mut mock = Mock::given(method("POST"))
        .and(path("/oauth/introspect"))
        .and(basic_auth(CLIENT_ID, CLIENT_SECRET))
        .and(body_string_contains(format!("token={token}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(body));
    if let Some(times) = times {
        mock = mock.expect(times);
    }
    mock.mount(redmine).await;
}

async fn mock_introspect_status(redmine: &wiremock::MockServer, token: &str, status: u16) {
    Mock::given(method("POST"))
        .and(path("/oauth/introspect"))
        .and(body_string_contains(format!("token={token}")))
        .respond_with(ResponseTemplate::new(status))
        .mount(redmine)
        .await;
}

async fn mock_current_user_for(
    redmine: &wiremock::MockServer,
    token: &str,
    id: u64,
    login: &str,
    times: Option<u64>,
) {
    let mut mock = Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .and(header("authorization", format!("Bearer {token}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "user": {
                "id": id,
                "login": login,
                "firstname": "First",
                "lastname": "Last",
                "mail": format!("{login}@example.com"),
                "created_on": "2024-01-01T00:00:00Z",
            }
        })));
    if let Some(times) = times {
        mock = mock.expect(times);
    }
    mock.mount(redmine).await;
}

fn initialize_body() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "oauth-test", "version": "0" }
        }
    })
}

fn raw_client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .expect("build a test HTTP client")
}

/// A raw `POST /mcp` `initialize` call, optionally with headers applied by
/// `configure`. Used for every test that asserts on the `401`/`503`
/// response itself rather than on tool-call behaviour.
async fn raw_initialize(
    harness: &support::HttpHarness,
    configure: impl FnOnce(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> reqwest::Response {
    let request = raw_client()
        .post(harness.mcp_url())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&initialize_body());
    configure(request)
        .send()
        .await
        .expect("request should complete")
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

#[tokio::test]
async fn valid_token_reaches_redmine_verbatim_as_authorization_bearer() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    const TOKEN: &str = "the-users-access-token";
    mock_introspect(
        &harness.redmine,
        TOKEN,
        serde_json::json!({ "active": true, "sub": "5", "username": "alice" }),
        None,
    )
    .await;
    mock_current_user_for(&harness.redmine, TOKEN, 5, "alice", None).await;

    let client = connect_with_token(&harness, TOKEN).await;
    let result = client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await
        .expect("get_current_user should succeed with a valid token");
    let text = result
        .content
        .iter()
        .filter_map(rmcp::model::ContentBlock::as_text)
        .map(|t| t.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let body: serde_json::Value =
        serde_json::from_str(text.lines().last().unwrap()).expect("last block is the JSON body");
    assert_eq!(body["login"], "alice");
    client.cancel().await.ok();
}

#[tokio::test]
async fn no_token_is_401_with_the_resource_metadata_challenge_and_zero_upstream_hits() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    // Any hit at all — introspection included — is a bug: no request should
    // leave this server before the header is checked.
    Mock::given(method("POST"))
        .and(path("/oauth/introspect"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&harness.redmine)
        .await;

    let response = raw_initialize(&harness, |r| r).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let challenge = response
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .expect("401 must carry WWW-Authenticate")
        .to_string();
    assert!(challenge.starts_with("Bearer resource_metadata="));
    assert!(challenge.contains("/.well-known/oauth-protected-resource/mcp"));
    assert!(!challenge.contains("error="));
}

#[tokio::test]
async fn unauthenticated_routes_are_never_401d_in_oauth_mode() {
    // O8: the middleware is mounted on the MCP route only. Every one of
    // these must stay reachable with no bearer token, even though `/mcp`
    // itself requires one in this mode. `REDMINE_MCP_ALLOWED_ORIGINS` is set
    // so the CORS preflight check below actually exercises the CORS layer
    // (which answers `OPTIONS` itself, outside the auth middleware) rather
    // than the no-CORS-configured default.
    let harness = support::http_harness(&oauth_env(&[(
        "REDMINE_MCP_ALLOWED_ORIGINS",
        "https://app.example.com",
    )]))
    .await;
    let client = raw_client();

    for path in ["/livez", "/readyz", "/health"] {
        let response = client
            .get(harness.url(path))
            .send()
            .await
            .expect("request should complete");
        assert_ne!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{path} must not require a bearer token"
        );
    }

    let files_response = client
        .get(harness.url("/files/00000000-0000-0000-0000-000000000000"))
        .send()
        .await
        .expect("request should complete");
    assert_ne!(files_response.status(), StatusCode::UNAUTHORIZED);

    let well_known_response = client
        .get(harness.url("/.well-known/oauth-protected-resource/mcp"))
        .send()
        .await
        .expect("request should complete");
    assert_ne!(well_known_response.status(), StatusCode::UNAUTHORIZED);

    let preflight_response = client
        .request(reqwest::Method::OPTIONS, harness.mcp_url())
        .header("origin", "https://app.example.com")
        .header("access-control-request-method", "POST")
        .send()
        .await
        .expect("request should complete");
    assert_ne!(preflight_response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn initialize_itself_requires_a_token() {
    // Pinned deliberately (O1's consequence): unlike every other auth mode,
    // `initialize` is not exempt in `oauth` mode.
    let harness = support::http_harness(&oauth_env(&[])).await;
    let response = raw_initialize(&harness, |r| r).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_duplicated_authorization_header_is_401_invalid_request() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    Mock::given(method("POST"))
        .and(path("/oauth/introspect"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&harness.redmine)
        .await;

    let response = raw_initialize(&harness, |r| {
        r.header("authorization", "Bearer one")
            .header("authorization", "Bearer two")
    })
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let challenge = response
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(challenge.contains(r#"error="invalid_request""#));
}

#[tokio::test]
async fn a_non_bearer_scheme_is_401_invalid_request() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    let response = raw_initialize(&harness, |r| {
        r.header("authorization", "Basic dXNlcjpwYXNz")
    })
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let challenge = response
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(challenge.contains(r#"error="invalid_request""#));
}

#[tokio::test]
async fn an_oversized_token_is_401_invalid_request_with_zero_upstream_hits() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    Mock::given(method("POST"))
        .and(path("/oauth/introspect"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&harness.redmine)
        .await;

    let value = format!("Bearer {}", "a".repeat(5000));
    let response = raw_initialize(&harness, |r| r.header("authorization", value)).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_inactive_token_is_401_invalid_token() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    const TOKEN: &str = "revoked-or-unknown-token";
    mock_introspect(
        &harness.redmine,
        TOKEN,
        serde_json::json!({ "active": false }),
        None,
    )
    .await;

    let response = raw_initialize(&harness, |r| {
        r.header("authorization", format!("Bearer {TOKEN}"))
    })
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let challenge = response
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(challenge.contains(r#"error="invalid_token""#));
}

#[tokio::test]
async fn an_active_but_expired_token_is_401_invalid_token() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    const TOKEN: &str = "expired-token";
    let past = chrono::Utc::now().timestamp() - 3600;
    mock_introspect(
        &harness.redmine,
        TOKEN,
        serde_json::json!({ "active": true, "exp": past }),
        None,
    )
    .await;

    let response = raw_initialize(&harness, |r| {
        r.header("authorization", format!("Bearer {TOKEN}"))
    })
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let challenge = response
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(challenge.contains(r#"error="invalid_token""#));
}

#[tokio::test]
async fn introspection_5xx_is_503_with_retry_after_never_401() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    const TOKEN: &str = "some-token";
    mock_introspect_status(&harness.redmine, TOKEN, 500).await;

    let response = raw_initialize(&harness, |r| {
        r.header("authorization", format!("Bearer {TOKEN}"))
    })
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()),
        Some("5")
    );
}

#[tokio::test]
async fn introspection_rejecting_our_own_client_credentials_is_503_never_401() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    const TOKEN: &str = "some-token";
    mock_introspect_status(&harness.redmine, TOKEN, 401).await;

    let response = raw_initialize(&harness, |r| {
        r.header("authorization", format!("Bearer {TOKEN}"))
    })
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn introspection_route_not_found_is_503_never_401() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    const TOKEN: &str = "some-token";
    mock_introspect_status(&harness.redmine, TOKEN, 404).await;

    let response = raw_initialize(&harness, |r| {
        r.header("authorization", format!("Bearer {TOKEN}"))
    })
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn two_tool_calls_with_the_same_token_perform_one_introspection() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    const TOKEN: &str = "cached-token";
    mock_introspect(
        &harness.redmine,
        TOKEN,
        serde_json::json!({ "active": true, "sub": "5", "username": "alice" }),
        Some(1),
    )
    .await;
    mock_current_user_for(&harness.redmine, TOKEN, 5, "alice", Some(2)).await;

    let client = connect_with_token(&harness, TOKEN).await;
    for _ in 0..2 {
        client
            .call_tool(CallToolRequestParams::new("get_current_user"))
            .await
            .expect("get_current_user should succeed");
    }
    client.cancel().await.ok();
    // Dropping the harness verifies wiremock's `expect(1)` on introspection.
}

#[tokio::test]
async fn a_zero_cache_ttl_introspects_more_than_once_across_two_calls() {
    // Unlike the cached case above (exactly one introspection for the whole
    // session), `ttl=0` must re-introspect on every request that reaches the
    // middleware. This only asserts "more than once", not an exact count,
    // since the transport itself may issue more than one HTTP request per
    // logical tool call (e.g. opening its event stream).
    let harness = support::http_harness(&oauth_env(&[(
        "REDMINE_OAUTH_TOKEN_CACHE_TTL_SECONDS",
        "0",
    )]))
    .await;
    const TOKEN: &str = "uncached-token";
    mock_introspect(
        &harness.redmine,
        TOKEN,
        serde_json::json!({ "active": true, "sub": "5", "username": "alice" }),
        None,
    )
    .await;
    mock_current_user_for(&harness.redmine, TOKEN, 5, "alice", Some(2)).await;

    let client = connect_with_token(&harness, TOKEN).await;
    for _ in 0..2 {
        client
            .call_tool(CallToolRequestParams::new("get_current_user"))
            .await
            .expect("get_current_user should succeed");
    }
    client.cancel().await.ok();

    let requests = harness
        .redmine
        .received_requests()
        .await
        .expect("request recording should be enabled");
    let introspections = requests
        .iter()
        .filter(|r| r.url.path() == "/oauth/introspect")
        .count();
    assert!(
        introspections >= 2,
        "expected at least one introspection per tool call, got {introspections}"
    );
}

#[tokio::test]
async fn two_concurrent_tokens_never_cross_contaminate_identity() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    const TOKEN_ALICE: &str = "alice-token";
    const TOKEN_BOB: &str = "bob-token";
    mock_introspect(
        &harness.redmine,
        TOKEN_ALICE,
        serde_json::json!({ "active": true, "sub": "1", "username": "alice" }),
        None,
    )
    .await;
    mock_introspect(
        &harness.redmine,
        TOKEN_BOB,
        serde_json::json!({ "active": true, "sub": "2", "username": "bob" }),
        None,
    )
    .await;
    mock_current_user_for(&harness.redmine, TOKEN_ALICE, 1, "alice", None).await;
    mock_current_user_for(&harness.redmine, TOKEN_BOB, 2, "bob", None).await;

    let alice = connect_with_token(&harness, TOKEN_ALICE).await;
    let bob = connect_with_token(&harness, TOKEN_BOB).await;

    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..5 {
        let alice_result = alice.call_tool(CallToolRequestParams::new("get_current_user"));
        let bob_result = bob.call_tool(CallToolRequestParams::new("get_current_user"));
        let (a, b) = tokio::join!(alice_result, bob_result);
        calls.spawn(async move { (a, b) });
    }
    while let Some(joined) = calls.join_next().await {
        let (a, b) = joined.expect("task should not panic");
        let a = a.expect("alice's call should succeed");
        let b = b.expect("bob's call should succeed");
        let a_text = a
            .content
            .iter()
            .filter_map(rmcp::model::ContentBlock::as_text)
            .map(|t| t.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let b_text = b
            .content
            .iter()
            .filter_map(rmcp::model::ContentBlock::as_text)
            .map(|t| t.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let a_body: serde_json::Value =
            serde_json::from_str(a_text.lines().last().unwrap()).unwrap();
        let b_body: serde_json::Value =
            serde_json::from_str(b_text.lines().last().unwrap()).unwrap();
        assert_eq!(a_body["login"], "alice");
        assert_eq!(b_body["login"], "bob");
    }
    alice.cancel().await.ok();
    bob.cancel().await.ok();
}

#[derive(Clone, Default)]
struct SharedBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedBuf {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The access token, the introspection client secret, and the form body's
/// `token=` field must never appear in captured `TRACE` output, across a
/// success, a `401` (invalid token), and a `503` (introspection down).
/// Mirrors `auth_per_user.rs`'s equivalent test for the `X-Redmine-API-Key`
/// header. (Manually verified this assertion fails if a
/// `tracing::debug!(?parts)` is added to the bearer-auth middleware; removed
/// after confirming.)
#[tokio::test]
async fn no_secret_appears_in_captured_trace_logs() {
    let buf = SharedBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_max_level(tracing::Level::TRACE)
        .without_time()
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);

    let harness = support::http_harness(&oauth_env(&[])).await;
    const SUCCESS_TOKEN: &str = "super-secret-success-path-token-0123456789";
    const INVALID_TOKEN: &str = "super-secret-invalid-path-token-abcdefghijk";
    const UNAVAILABLE_TOKEN: &str = "super-secret-unavailable-path-token-zyxwvu";

    mock_introspect(
        &harness.redmine,
        SUCCESS_TOKEN,
        serde_json::json!({ "active": true, "sub": "5", "username": "alice" }),
        None,
    )
    .await;
    mock_current_user_for(&harness.redmine, SUCCESS_TOKEN, 5, "alice", None).await;
    mock_introspect(
        &harness.redmine,
        INVALID_TOKEN,
        serde_json::json!({ "active": false }),
        None,
    )
    .await;
    mock_introspect_status(&harness.redmine, UNAVAILABLE_TOKEN, 500).await;

    let success_client = connect_with_token(&harness, SUCCESS_TOKEN).await;
    success_client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await
        .expect("success path should succeed");
    success_client.cancel().await.ok();

    let invalid_response = raw_initialize(&harness, |r| {
        r.header("authorization", format!("Bearer {INVALID_TOKEN}"))
    })
    .await;
    assert_eq!(invalid_response.status(), StatusCode::UNAUTHORIZED);

    let unavailable_response = raw_initialize(&harness, |r| {
        r.header("authorization", format!("Bearer {UNAVAILABLE_TOKEN}"))
    })
    .await;
    assert_eq!(
        unavailable_response.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );

    drop(guard);

    let captured = String::from_utf8(buf.0.lock().unwrap().clone()).expect("logs are valid UTF-8");
    for secret in [
        SUCCESS_TOKEN,
        INVALID_TOKEN,
        UNAVAILABLE_TOKEN,
        CLIENT_SECRET,
    ] {
        assert!(
            !captured.contains(secret),
            "captured TRACE log leaked a secret {secret:?}: {captured}"
        );
    }
    assert!(
        !captured.contains("token="),
        "captured TRACE log leaked the introspection form body: {captured}"
    );
}

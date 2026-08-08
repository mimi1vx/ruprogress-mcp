//! End-to-end coverage for `AuthMode::LegacyPerUser`: each request carries
//! its own credential, forwarded verbatim; no cross-request bleed through the
//! shared connection pool; a missing/malformed/duplicated header is rejected
//! before any Redmine request is attempted; and the key never appears in a
//! captured log line, an error `Display`, or an error `Debug`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::sync::{Arc, Mutex};

use reqwest::Client as ReqwestClient;
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceExt as _};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, ResponseTemplate};

fn per_user_env() -> Vec<(&'static str, &'static str)> {
    vec![
        ("REDMINE_AUTH_MODE", "legacy-per-user"),
        ("REDMINE_PER_USER_TRUST_PROXY", "true"),
    ]
}

fn content_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(rmcp::model::ContentBlock::as_text)
        .map(|t| t.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

fn body_of(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    let text = content_text(result);
    serde_json::from_str(text.lines().last().expect("at least one content line"))
        .expect("last content block is JSON")
}

/// Connects with a fresh `reqwest::Client` carrying exactly `headers` on
/// every request — bypassing rmcp's `custom_headers` (a single-value-per-name
/// `HashMap`) so a test can send a header more than once, which
/// `HeaderMap::append` supports and `custom_headers` does not.
async fn connect_with_headers(
    mcp_url: &str,
    headers: http::HeaderMap,
) -> RunningService<RoleClient, ()> {
    let client = ReqwestClient::builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client should build");
    let config = StreamableHttpClientTransportConfig::with_uri(mcp_url.to_string());
    let transport = StreamableHttpClientTransport::with_client(client, config);
    ().serve(transport)
        .await
        .expect("client should connect over streamable HTTP")
}

async fn connect_with_key(mcp_url: &str, key: &str) -> RunningService<RoleClient, ()> {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "x-redmine-api-key",
        http::HeaderValue::from_str(key).expect("test key is a valid header value"),
    );
    connect_with_headers(mcp_url, headers).await
}

async fn connect_with_no_headers(mcp_url: &str) -> RunningService<RoleClient, ()> {
    connect_with_headers(mcp_url, http::HeaderMap::new()).await
}

/// Sends one raw `tools/call` JSON-RPC request over a plain `reqwest`
/// request, bypassing rmcp's client transport. Needed only to send the same
/// header name more than once: `reqwest::ClientBuilder::default_headers`
/// silently keeps just the first value for a repeated name (a `reqwest`
/// quirk, verified independently — not an MCP or `ruprogress-mcp` behaviour),
/// while two `RequestBuilder::header()` calls on one request send both.
/// The stateless transport accepts a bare `tools/call` with no prior
/// `initialize` (it synthesises default negotiation params per request).
async fn call_tool_raw(
    mcp_url: &str,
    header_pairs: &[(&str, &str)],
    tool_name: &str,
) -> serde_json::Value {
    let mut builder = ReqwestClient::new()
        .post(mcp_url)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json");
    for (name, value) in header_pairs {
        builder = builder.header(*name, *value);
    }
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": tool_name, "arguments": {}}
    });
    builder
        .json(&body)
        .send()
        .await
        .expect("raw POST should reach the server")
        .json()
        .await
        .expect("response body should be JSON")
}

fn user_fixture(id: u64, login: &str) -> serde_json::Value {
    serde_json::json!({
        "user": {
            "id": id,
            "login": login,
            "firstname": "First",
            "lastname": "Last",
            "mail": format!("{login}@example.com"),
            "created_on": "2024-01-01T00:00:00Z",
            "last_login_on": "2026-08-01T00:00:00Z",
        }
    })
}

#[tokio::test]
async fn forwards_the_inbound_api_key_verbatim() {
    let harness = support::http_harness(&per_user_env()).await;
    Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .and(header("x-redmine-api-key", "the-inbound-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(user_fixture(1, "alice")))
        .expect(1)
        .mount(&harness.redmine)
        .await;

    let client = connect_with_key(&harness.mcp_url(), "the-inbound-key").await;
    let result = client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await
        .expect("get_current_user should succeed");
    assert_eq!(body_of(&result)["login"], "alice");
    client.cancel().await.ok();
}

/// Guards against pool-sharing credential bleed: two identities issue
/// interleaved requests against the one pooled `reqwest::Client` this server
/// holds, and each must only ever see its own
/// user.
#[tokio::test]
async fn two_concurrent_callers_with_different_keys_see_only_their_own_user() {
    let harness = support::http_harness(&per_user_env()).await;
    Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .and(header("x-redmine-api-key", "key-a"))
        .respond_with(ResponseTemplate::new(200).set_body_json(user_fixture(10, "user-a")))
        .mount(&harness.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .and(header("x-redmine-api-key", "key-b"))
        .respond_with(ResponseTemplate::new(200).set_body_json(user_fixture(20, "user-b")))
        .mount(&harness.redmine)
        .await;

    let client_a = Arc::new(connect_with_key(&harness.mcp_url(), "key-a").await);
    let client_b = Arc::new(connect_with_key(&harness.mcp_url(), "key-b").await);

    let mut rounds = Vec::new();
    for _ in 0..10 {
        let (a, b) = (Arc::clone(&client_a), Arc::clone(&client_b));
        rounds.push(tokio::spawn(async move {
            tokio::join!(
                a.call_tool(CallToolRequestParams::new("get_current_user")),
                b.call_tool(CallToolRequestParams::new("get_current_user")),
            )
        }));
    }
    for round in rounds {
        let (result_a, result_b) = round.await.expect("task should not panic");
        assert_eq!(
            body_of(&result_a.expect("client A should succeed"))["login"],
            "user-a"
        );
        assert_eq!(
            body_of(&result_b.expect("client B should succeed"))["login"],
            "user-b"
        );
    }
    // `RunningService::cancel` takes `self` by value, which an `Arc`-shared
    // handle can't offer; `Drop` tears the connection down once both `Arc`s
    // go out of scope at the end of the test.
}

#[tokio::test]
async fn missing_header_is_rejected_and_no_redmine_request_is_made() {
    let harness = support::http_harness(&per_user_env()).await;
    let client = connect_with_no_headers(&harness.mcp_url()).await;

    let error = client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await
        .expect_err("a missing credential header must be rejected");
    assert!(
        format!("{error}").contains("X-Redmine-API-Key"),
        "unexpected error: {error}"
    );

    let received = harness
        .redmine
        .received_requests()
        .await
        .unwrap_or_default();
    assert!(
        received.is_empty(),
        "no Redmine request should have been made, got: {received:?}"
    );
    client.cancel().await.ok();
}

#[tokio::test]
async fn duplicated_header_is_rejected() {
    let harness = support::http_harness(&per_user_env()).await;
    let response = call_tool_raw(
        &harness.mcp_url(),
        &[
            ("x-redmine-api-key", "key-one"),
            ("x-redmine-api-key", "key-two"),
        ],
        "get_current_user",
    )
    .await;
    let message = response["error"]["message"]
        .as_str()
        .expect("a duplicated credential header must be rejected with a JSON-RPC error");
    assert!(
        message.contains("exactly once"),
        "unexpected error: {message}"
    );

    let received = harness
        .redmine
        .received_requests()
        .await
        .unwrap_or_default();
    assert!(
        received.is_empty(),
        "no Redmine request should have been made, got: {received:?}"
    );
}

#[tokio::test]
async fn readyz_reports_not_probed_and_does_not_500() {
    let harness = support::http_harness(&per_user_env()).await;
    let response = ReqwestClient::new()
        .get(harness.url("/readyz"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(body["status"], "ready");
    assert_eq!(body["redmine"], "not_probed");
}

#[tokio::test]
async fn audit_identity_logs_one_fingerprint_line_per_tool_call() {
    let buf = SharedBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_max_level(tracing::Level::INFO)
        .without_time()
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);

    let mut env = per_user_env();
    env.push(("REDMINE_PER_USER_AUDIT_IDENTITY", "true"));
    let harness = support::http_harness(&env).await;
    let key = "audited-caller-key";
    Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .and(header("x-redmine-api-key", key))
        .respond_with(ResponseTemplate::new(200).set_body_json(user_fixture(1, "alice")))
        .mount(&harness.redmine)
        .await;

    let client = connect_with_key(&harness.mcp_url(), key).await;
    for _ in 0..3 {
        client
            .call_tool(CallToolRequestParams::new("get_current_user"))
            .await
            .expect("get_current_user should succeed");
    }
    client.cancel().await.ok();
    drop(guard);

    let captured = String::from_utf8(buf.0.lock().unwrap().clone()).expect("logs are valid UTF-8");
    let lines = captured.matches("per-user request").count();
    assert_eq!(
        lines, 3,
        "expected one fingerprint line per tool call, got:\n{captured}"
    );
    assert!(
        !captured.contains(key),
        "audit log leaked the API key: {captured}"
    );
}

#[tokio::test]
async fn tools_list_and_initialize_succeed_without_any_credential_header() {
    let harness = support::http_harness(&per_user_env()).await;
    let client = connect_with_no_headers(&harness.mcp_url()).await;

    let info = client.peer_info().expect("server info after initialize");
    assert!(!info.protocol_version.to_string().is_empty());

    let tools = client
        .list_all_tools()
        .await
        .expect("tools/list should succeed with no credential header");
    assert!(!tools.is_empty());
    client.cancel().await.ok();
}

/// Sibling of `tools_basic.rs`'s
/// `get_mcp_server_info_reports_current_user_null_when_redmine_unreachable`,
/// which asserts `"legacy"` for the default legacy mode.
#[tokio::test]
async fn get_mcp_server_info_reports_legacy_per_user_auth_mode() {
    let harness = support::http_harness(&per_user_env()).await;
    let client = connect_with_no_headers(&harness.mcp_url()).await;

    let result = client
        .call_tool(CallToolRequestParams::new("get_mcp_server_info"))
        .await
        .expect("get_mcp_server_info should succeed even without a credential header");
    let body = body_of(&result);
    assert_eq!(body["auth_mode"], "legacy-per-user");
    assert_eq!(body["current_user"], serde_json::Value::Null);
    client.cancel().await.ok();
}

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

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

/// A stray `tracing::debug!(?parts)`/`?ctx` anywhere on the per-user request
/// path would print the key verbatim, since inbound HTTP headers are not
/// marked `set_sensitive` the way outbound ones are — this is the finding a
/// future change is most likely to reintroduce. This asserts on *captured*
/// `TRACE`-level output across a success, a Redmine 401, and a rejected
/// (duplicated) header, rather than on the absence of a source-code pattern,
/// which would not catch a regression. (Manually verified this assertion
/// fails if a `tracing::debug!(?parts)` is added to
/// `auth::per_user::credential`; removed after confirming.)
#[tokio::test]
async fn the_api_key_never_appears_in_captured_logs_or_error_output() {
    let buf = SharedBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_max_level(tracing::Level::TRACE)
        .without_time()
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);

    let harness = support::http_harness(&per_user_env()).await;
    let success_key = "super-secret-success-path-key-0123456789";
    let unauthorized_key = "super-secret-unauthorized-path-key-abcdef";
    let rejected_key_one = "super-secret-rejected-key-one-000000";
    let rejected_key_two = "super-secret-rejected-key-two-111111";

    Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .and(header("x-redmine-api-key", success_key))
        .respond_with(ResponseTemplate::new(200).set_body_json(user_fixture(1, "alice")))
        .mount(&harness.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .and(header("x-redmine-api-key", unauthorized_key))
        .respond_with(ResponseTemplate::new(401))
        .mount(&harness.redmine)
        .await;

    let success_client = connect_with_key(&harness.mcp_url(), success_key).await;
    let success_result = success_client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await
        .expect("success path should succeed");
    assert_ne!(success_result.is_error, Some(true));
    success_client.cancel().await.ok();

    let unauthorized_client = connect_with_key(&harness.mcp_url(), unauthorized_key).await;
    let unauthorized_result = unauthorized_client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await
        .expect("call_tool itself should succeed; the 401 is an in-band tool error");
    assert_eq!(unauthorized_result.is_error, Some(true));
    unauthorized_client.cancel().await.ok();

    let rejected_response = call_tool_raw(
        &harness.mcp_url(),
        &[
            ("x-redmine-api-key", rejected_key_one),
            ("x-redmine-api-key", rejected_key_two),
        ],
        "get_current_user",
    )
    .await;
    assert!(
        rejected_response["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("exactly once")),
        "duplicated header should be rejected: {rejected_response}"
    );

    drop(guard);

    let captured = String::from_utf8(buf.0.lock().unwrap().clone()).expect("logs are valid UTF-8");
    let rejected_debug = format!("{rejected_response:?}");

    for secret in [
        success_key,
        unauthorized_key,
        rejected_key_one,
        rejected_key_two,
    ] {
        assert!(
            !captured.contains(secret),
            "captured TRACE log leaked the API key {secret:?}: {captured}"
        );
        assert!(
            !rejected_debug.contains(secret),
            "rejected-header error response leaked the API key {secret:?}: {rejected_debug}"
        );
    }
}

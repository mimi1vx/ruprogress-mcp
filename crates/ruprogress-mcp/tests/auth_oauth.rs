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

/// Like [`mock_introspect`] but with a `scope` field, for the
/// scope-enforcement tests. `scope` is RFC 7662's space-delimited string;
/// pass `""` for a token with no scopes at all.
async fn mock_introspect_scoped(redmine: &wiremock::MockServer, token: &str, scope: &str) {
    mock_introspect(
        redmine,
        token,
        serde_json::json!({
            "active": true,
            "sub": "5",
            "username": "alice",
            "scope": scope,
        }),
        None,
    )
    .await;
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

/// `tools/call` `tool` with `args`, over a fresh connection authenticated as
/// `token`. Used by the scope-enforcement tests below.
async fn call_with_token(
    harness: &support::HttpHarness,
    token: &str,
    tool: &str,
    args: serde_json::Value,
) -> rmcp::model::CallToolResult {
    let client = connect_with_token(harness, token).await;
    let mut request = CallToolRequestParams::new(tool.to_string());
    request.arguments = args.as_object().cloned();
    let result = client
        .call_tool(request)
        .await
        .expect("call_tool should succeed at the protocol level (an in-band error is Ok)");
    client.cancel().await.ok();
    result
}

/// `tools/list`'s tool names, over a fresh connection authenticated as
/// `token`.
async fn list_tool_names(harness: &support::HttpHarness, token: &str) -> Vec<String> {
    let client = connect_with_token(harness, token).await;
    let tools = client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    let names = tools.tools.iter().map(|t| t.name.to_string()).collect();
    client.cancel().await.ok();
    names
}

#[tokio::test]
async fn a_view_issues_only_token_sees_and_can_only_call_that_scopes_tools() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    const TOKEN: &str = "view-issues-only-token";
    mock_introspect_scoped(&harness.redmine, TOKEN, "view_issues").await;

    let names = list_tool_names(&harness, TOKEN).await;
    assert!(names.iter().any(|n| n == "list_redmine_issues"));
    assert!(!names.iter().any(|n| n == "create_redmine_issue"));

    // Any hit at all means the scope check let a request through before it
    // should have.
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&harness.redmine)
        .await;

    let denied = call_with_token(
        &harness,
        TOKEN,
        "create_redmine_issue",
        serde_json::json!({"project_id": 1, "subject": "x"}),
    )
    .await;
    assert_eq!(denied.is_error, Some(true));
    let structured = denied.structured_content.expect("structured error");
    assert_eq!(structured["code"], "INSUFFICIENT_SCOPE");
    assert!(structured["error"].as_str().unwrap().contains("add_issues"));
}

#[tokio::test]
async fn an_admin_token_sees_and_can_call_every_tool() {
    let harness =
        support::http_harness(&oauth_env(&[("REDMINE_MCP_EXPOSE_ADMIN_TOOLS", "true")])).await;
    const TOKEN: &str = "admin-token";
    mock_introspect_scoped(&harness.redmine, TOKEN, "admin").await;

    let names = list_tool_names(&harness, TOKEN).await;
    assert_eq!(names.len(), 42, "expected every registered tool: {names:?}");

    // A write tool with a non-trivial scope requirement (add_issues):
    // admin bypasses TOOL_SCOPES entirely, not just the visibility check.
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(201).set_body_json(sample_issue_body(1)))
        .mount(&harness.redmine)
        .await;
    let result = call_with_token(
        &harness,
        TOKEN,
        "create_redmine_issue",
        serde_json::json!({"project_id": 1, "subject": "x"}),
    )
    .await;
    assert_eq!(result.is_error, Some(false));
}

#[tokio::test]
async fn an_empty_scope_token_sees_and_calls_only_unscoped_tools() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    const TOKEN: &str = "empty-scope-token";
    mock_introspect_scoped(&harness.redmine, TOKEN, "").await;
    mock_current_user_for(&harness.redmine, TOKEN, 5, "alice", None).await;

    let names = list_tool_names(&harness, TOKEN).await;
    assert!(names.iter().any(|n| n == "get_current_user"));
    assert!(!names.iter().any(|n| n == "list_redmine_projects"));

    let result = call_with_token(&harness, TOKEN, "get_current_user", serde_json::json!({})).await;
    assert_eq!(result.is_error, Some(false));
}

#[tokio::test]
async fn manage_issue_relation_enforces_scope_per_action() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    const TOKEN: &str = "view-issues-relation-token";
    mock_introspect_scoped(&harness.redmine, TOKEN, "view_issues").await;
    Mock::given(method("GET"))
        .and(path("/issues/1/relations.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"relations": []})),
        )
        .mount(&harness.redmine)
        .await;
    Mock::given(method("POST"))
        .and(path("/issues/1/relations.json"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&harness.redmine)
        .await;

    let list_result = call_with_token(
        &harness,
        TOKEN,
        "manage_issue_relation",
        serde_json::json!({"action": "list", "issue_id": 1}),
    )
    .await;
    assert_eq!(list_result.is_error, Some(false));

    let create_result = call_with_token(
        &harness,
        TOKEN,
        "manage_issue_relation",
        serde_json::json!({"action": "create", "issue_id": 1, "issue_to_id": 2}),
    )
    .await;
    assert_eq!(create_result.is_error, Some(true));
    assert_eq!(
        create_result.structured_content.unwrap()["code"],
        "INSUFFICIENT_SCOPE"
    );
}

fn sample_issue_body(id: u64) -> serde_json::Value {
    serde_json::json!({
        "issue": {
            "id": id, "project": {"id": 1, "name": "P"}, "tracker": {"id": 1, "name": "Bug"},
            "status": {"id": 1, "name": "New"}, "priority": {"id": 1, "name": "Normal"},
            "author": {"id": 1, "name": "A"}, "subject": "s",
            "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
        }
    })
}

#[tokio::test]
async fn update_redmine_issue_notes_only_carve_out_end_to_end() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    const TOKEN: &str = "add-issue-notes-token";
    mock_introspect_scoped(&harness.redmine, TOKEN, "add_issue_notes").await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&harness.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sample_issue_body(7)))
        .mount(&harness.redmine)
        .await;

    let notes_only = call_with_token(
        &harness,
        TOKEN,
        "update_redmine_issue",
        serde_json::json!({"issue_id": 7, "notes": "a comment"}),
    )
    .await;
    assert_eq!(notes_only.is_error, Some(false));

    let notes_plus_field = call_with_token(
        &harness,
        TOKEN,
        "update_redmine_issue",
        serde_json::json!({"issue_id": 7, "notes": "a comment", "subject": "new subject"}),
    )
    .await;
    assert_eq!(notes_plus_field.is_error, Some(true));
    assert_eq!(
        notes_plus_field.structured_content.unwrap()["code"],
        "INSUFFICIENT_SCOPE"
    );

    let notes_plus_upload = call_with_token(
        &harness,
        TOKEN,
        "update_redmine_issue",
        serde_json::json!({"issue_id": 7, "notes": "a comment", "uploads": []}),
    )
    .await;
    assert_eq!(notes_plus_upload.is_error, Some(true));
    assert_eq!(
        notes_plus_upload.structured_content.unwrap()["code"],
        "INSUFFICIENT_SCOPE"
    );
}

#[tokio::test]
async fn update_redmine_issue_agile_field_requires_view_agile_queries_end_to_end() {
    let harness = support::http_harness(&oauth_env(&[("REDMINE_AGILE_ENABLED", "true")])).await;
    const TOKEN: &str = "edit-issues-only-token";
    mock_introspect_scoped(&harness.redmine, TOKEN, "edit_issues").await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&harness.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sample_issue_body(7)))
        .mount(&harness.redmine)
        .await;

    let subject_only = call_with_token(
        &harness,
        TOKEN,
        "update_redmine_issue",
        serde_json::json!({"issue_id": 7, "subject": "new subject"}),
    )
    .await;
    assert_eq!(subject_only.is_error, Some(false));

    let with_story_points = call_with_token(
        &harness,
        TOKEN,
        "update_redmine_issue",
        serde_json::json!({"issue_id": 7, "story_points": 8}),
    )
    .await;
    assert_eq!(with_story_points.is_error, Some(true));
    assert_eq!(
        with_story_points.structured_content.unwrap()["code"],
        "INSUFFICIENT_SCOPE"
    );
}

#[tokio::test]
async fn update_redmine_issue_tag_list_requires_edit_issues_and_a_tag_scope_end_to_end() {
    let harness = support::http_harness(&oauth_env(&[("REDMINE_TAGS_ENABLED", "true")])).await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&harness.redmine)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sample_issue_body(7)))
        .mount(&harness.redmine)
        .await;

    const NEITHER_TOKEN: &str = "edit-issues-only-token";
    mock_introspect_scoped(&harness.redmine, NEITHER_TOKEN, "edit_issues").await;
    let denied = call_with_token(
        &harness,
        NEITHER_TOKEN,
        "update_redmine_issue",
        serde_json::json!({"issue_id": 7, "tag_list": ["a"]}),
    )
    .await;
    assert_eq!(denied.is_error, Some(true));
    assert_eq!(
        denied.structured_content.unwrap()["code"],
        "INSUFFICIENT_SCOPE"
    );

    const CREATE_TAGS_TOKEN: &str = "edit-issues-create-tags-token";
    mock_introspect_scoped(
        &harness.redmine,
        CREATE_TAGS_TOKEN,
        "edit_issues create_issue_tags",
    )
    .await;
    let passes_with_create = call_with_token(
        &harness,
        CREATE_TAGS_TOKEN,
        "update_redmine_issue",
        serde_json::json!({"issue_id": 7, "tag_list": ["a"]}),
    )
    .await;
    assert_eq!(passes_with_create.is_error, Some(false));

    const EDIT_TAGS_TOKEN: &str = "edit-issues-edit-tags-token";
    mock_introspect_scoped(
        &harness.redmine,
        EDIT_TAGS_TOKEN,
        "edit_issues edit_issue_tags",
    )
    .await;
    let passes_with_edit = call_with_token(
        &harness,
        EDIT_TAGS_TOKEN,
        "update_redmine_issue",
        serde_json::json!({"issue_id": 7, "tag_list": ["a"]}),
    )
    .await;
    assert_eq!(passes_with_edit.is_error, Some(false));

    // A non-tag update is unaffected: `edit_issues` alone is still enough.
    const EDIT_ONLY_TOKEN: &str = "edit-issues-non-tag-token";
    mock_introspect_scoped(&harness.redmine, EDIT_ONLY_TOKEN, "edit_issues").await;
    let non_tag_update = call_with_token(
        &harness,
        EDIT_ONLY_TOKEN,
        "update_redmine_issue",
        serde_json::json!({"issue_id": 7, "subject": "new subject"}),
    )
    .await;
    assert_eq!(non_tag_update.is_error, Some(false));
}

#[tokio::test]
async fn scope_enforcement_off_restores_unfiltered_visibility_and_calls() {
    let harness =
        support::http_harness(&oauth_env(&[("REDMINE_OAUTH_SCOPE_ENFORCEMENT", "off")])).await;
    const TOKEN: &str = "low-scope-token";
    mock_introspect_scoped(&harness.redmine, TOKEN, "view_issues").await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(201).set_body_json(sample_issue_body(1)))
        .mount(&harness.redmine)
        .await;

    let names = list_tool_names(&harness, TOKEN).await;
    assert!(names.iter().any(|n| n == "create_redmine_issue"));

    let result = call_with_token(
        &harness,
        TOKEN,
        "create_redmine_issue",
        serde_json::json!({"project_id": 1, "subject": "x"}),
    )
    .await;
    assert_eq!(result.is_error, Some(false));
}

/// A raw `tools/list` call carrying SEP-1319 `_meta` naming protocol version
/// `2026-07-28` — the version [`rmcp::model::CacheScope`] hints require
/// (S8). Built by hand rather than through `rmcp`'s client SDK: the SDK
/// only attaches this `_meta` automatically on connections established via
/// `serve_directly` (an already-known-peer shortcut this server's stateless
/// HTTP transport never uses), not after a normal `initialize` round trip.
async fn raw_tools_list_at_protocol_version_2026(
    harness: &support::HttpHarness,
    token: &str,
) -> serde_json::Value {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
            }
        }
    });
    raw_client()
        .post(harness.mcp_url())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", "tools/list")
        .header("authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .expect("request should complete")
        .json()
        .await
        .expect("json body")
}

#[tokio::test]
async fn filtered_tools_list_reports_a_private_cache_scope() {
    let harness = support::http_harness(&oauth_env(&[])).await;
    const TOKEN: &str = "cache-scope-token";
    mock_introspect_scoped(&harness.redmine, TOKEN, "view_issues").await;

    let response = raw_tools_list_at_protocol_version_2026(&harness, TOKEN).await;
    assert_eq!(response["result"]["cacheScope"], "private");
}

#[tokio::test]
async fn unfiltered_tools_list_reports_a_public_cache_scope() {
    // Legacy mode: `scope_enforcement_active()` is always false outside
    // `oauth`, so `list_tools` takes the unfiltered path — same as the
    // macro-generated version — and keeps `CacheScope::Public`. No bearer
    // token is needed (or checked) outside `oauth` mode; the header is
    // harmless to send anyway.
    let harness = support::http_harness(&[]).await;
    let response = raw_tools_list_at_protocol_version_2026(&harness, "unused").await;
    assert_eq!(response["result"]["cacheScope"], "public");
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

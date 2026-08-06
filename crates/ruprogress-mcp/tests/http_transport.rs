//! Protocol-level e2e over streamable HTTP, using rmcp's own client.
//!
//! The contract these assert is that the transport is *transparent*: the same
//! server, the same tools, the same content as over stdio.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::StreamableHttpClientTransport;

async fn connect(
    harness: &support::HttpHarness,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let transport = StreamableHttpClientTransport::from_uri(harness.mcp_url());
    ().serve(transport)
        .await
        .expect("client should connect over streamable HTTP")
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

async fn tool_names(client: &rmcp::service::RunningService<rmcp::RoleClient, ()>) -> Vec<String> {
    let mut names: Vec<String> = client
        .list_all_tools()
        .await
        .expect("tools/list should succeed")
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn initialize_negotiates_a_protocol_version() {
    let harness = support::http_harness(&[]).await;
    let client = connect(&harness).await;
    let info = client.peer_info().expect("server info after initialize");
    assert!(!info.protocol_version.to_string().is_empty());
    client.cancel().await.ok();
}

#[tokio::test]
async fn tools_list_over_http_matches_tools_list_over_stdio() {
    let http = support::http_harness(&[]).await;
    let http_client = connect(&http).await;
    let over_http = tool_names(&http_client).await;

    let stdio = support::harness(&[]).await;
    let over_stdio = tool_names(&stdio.client).await;

    assert_eq!(over_http, over_stdio);
    assert!(!over_http.is_empty(), "the tool list should not be empty");
    http_client.cancel().await.ok();
}

#[tokio::test]
async fn get_current_user_returns_the_fixture_user_over_http() {
    let harness = support::http_harness(&[]).await;
    support::mock_current_user(&harness.redmine, None).await;
    let client = connect(&harness).await;

    let result = client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await
        .expect("get_current_user should succeed");

    let text = content_text(&result);
    let body: serde_json::Value =
        serde_json::from_str(text.lines().last().unwrap()).expect("last block is the JSON body");
    assert_eq!(body["id"], 5);
    assert_eq!(body["login"], "alice");
    client.cancel().await.ok();
}

#[tokio::test]
async fn get_mcp_server_info_reports_the_http_transport() {
    let harness = support::http_harness(&[]).await;
    support::mock_current_user(&harness.redmine, None).await;
    let client = connect(&harness).await;

    let result = client
        .call_tool(CallToolRequestParams::new("get_mcp_server_info"))
        .await
        .expect("get_mcp_server_info should succeed");

    let text = content_text(&result);
    let body: serde_json::Value =
        serde_json::from_str(text.lines().last().unwrap()).expect("last block is the JSON body");
    assert_eq!(body["transport"], "http");
    // The bind address and MCP path are for operators, not for the model.
    let rendered = body.to_string();
    assert!(!rendered.contains("127.0.0.1"), "{rendered}");
    assert!(!rendered.contains("/mcp"), "{rendered}");
    client.cancel().await.ok();
}

#[tokio::test]
async fn legacy_per_user_serves_over_http_but_tools_still_report_not_implemented() {
    // The config boundary: `legacy-per-user` is *constructible* on HTTP (it is
    // a `Conflict` on stdio), but the credential choke point has nothing to
    // hand out yet, so a tool call must fail loudly rather than silently fall
    // back to a server-owned key.
    let harness = support::http_harness(&[
        ("REDMINE_AUTH_MODE", "legacy-per-user"),
        ("REDMINE_PER_USER_TRUST_PROXY", "true"),
    ])
    .await;
    let client = connect(&harness).await;

    let error = client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await
        .expect_err("per-user auth is not implemented yet");
    assert!(
        format!("{error}").contains("not yet implemented"),
        "unexpected error: {error}"
    );
    client.cancel().await.ok();
}

#[tokio::test]
async fn read_only_mode_still_hides_write_tools_over_http() {
    let harness = support::http_harness(&[("REDMINE_MCP_READ_ONLY", "true")]).await;
    let client = connect(&harness).await;
    let names = tool_names(&client).await;

    for write_tool in ruprogress_mcp::readonly::write_tools::ALL {
        assert!(
            !names.iter().any(|n| n == write_tool),
            "{write_tool} should be hidden in read-only mode"
        );
    }

    // `write_tools::ALL` is empty today, so the loop above proves nothing on
    // its own. What is checkable now is that the gate lives in the router and
    // not the transport: read-only over HTTP must hide exactly what read-only
    // over stdio hides.
    let stdio = support::harness(&[("REDMINE_MCP_READ_ONLY", "true")]).await;
    assert_eq!(names, tool_names(&stdio.client).await);
    client.cancel().await.ok();
}

/// A shutdown signal must let a running tool call finish. This is the
/// behaviour that breaks if rmcp's cancellation token is ever wired to the
/// same signal as axum's graceful shutdown: rmcp aborts in-flight handlers,
/// so the request would come back 500 instead of completing.
#[tokio::test]
async fn a_shutdown_signal_lets_an_in_flight_tool_call_finish() {
    use std::time::Duration;
    use wiremock::matchers::{method, path};

    let harness = support::http_harness(&[]).await;
    wiremock::Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(600))
                .set_body_json(serde_json::json!({
                    "user": {
                        "id": 5, "login": "alice", "firstname": "Alice",
                        "lastname": "Example", "mail": "alice@example.com",
                        "created_on": "2024-01-01T00:00:00Z",
                        "last_login_on": "2026-08-01T00:00:00Z",
                    }
                })),
        )
        .mount(&harness.redmine)
        .await;

    let client = connect(&harness).await;
    let call = tokio::spawn(async move {
        let result = client
            .call_tool(CallToolRequestParams::new("get_current_user"))
            .await;
        (client, result)
    });

    // Signal while the tool call is still waiting on Redmine.
    tokio::time::sleep(Duration::from_millis(150)).await;
    harness.shutdown.cancel();

    let (client, result) = call.await.expect("the call task should not panic");
    let result = result.expect("an in-flight tool call must survive the drain");
    let text = content_text(&result);
    let body: serde_json::Value =
        serde_json::from_str(text.lines().last().unwrap()).expect("last block is the JSON body");
    assert_eq!(body["login"], "alice");
    client.cancel().await.ok();
}

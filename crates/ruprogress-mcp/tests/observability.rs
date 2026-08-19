//! `server.rs`'s `tool_call` span closes with exactly one record per call,
//! on the `stdio` transport exercised by `support::harness` here (the HTTP
//! transport shares the same `call_tool`, so this is not a per-transport
//! concern) — carrying `tool`, `request_id`, `outcome`, `duration_ms`, and
//! never an argument value.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use rmcp::ErrorData;
use rmcp::handler::server::router::tool::ToolRoute;
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, Tool};
use ruprogress_mcp::server::RedmineMcp;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test(flavor = "current_thread")]
async fn ok_and_error_calls_each_produce_exactly_one_tool_call_record() {
    let capture = support::capture("trace").await;
    let h = support::harness(&[]).await;
    support::mock_current_user(&h.redmine, Some(1)).await;
    Mock::given(method("GET"))
        .and(path("/issues/999.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.redmine)
        .await;

    h.client
        .call_tool(CallToolRequestParams::new("get_current_user"))
        .await
        .expect("get_current_user should succeed");

    let mut failing = CallToolRequestParams::new("get_redmine_issue");
    failing.arguments = json!({"issue_id": 999}).as_object().cloned();
    h.client
        .call_tool(failing)
        .await
        .expect("a 404 from Redmine is an in-band error, not a protocol error");

    let captured = capture.finish();
    assert!(
        captured.contains("capture armed"),
        "capture observed nothing — this test cannot prove anything: {captured}"
    );

    assert_eq!(
        captured.matches("tool_call{tool=get_current_user").count(),
        1,
        "expected exactly one tool_call record for get_current_user: {captured}"
    );
    assert!(captured.contains(r#"outcome="ok""#), "{captured}");

    assert_eq!(
        captured.matches("tool_call{tool=get_redmine_issue").count(),
        1,
        "expected exactly one tool_call record for get_redmine_issue: {captured}"
    );
    assert!(captured.contains(r#"outcome="error""#), "{captured}");
    assert!(captured.contains(r#"code="NOT_FOUND""#), "{captured}");
    assert!(captured.contains("duration_ms="), "{captured}");
}

fn panicking_tool_route() -> ToolRoute<RedmineMcp> {
    ToolRoute::new_dyn(
        Tool::new(
            "panics_for_testing",
            "panics unconditionally",
            Arc::default(),
        ),
        |_ctx| {
            Box::pin(async {
                panic!("deliberate observability test panic");
                #[allow(unreachable_code)]
                Ok::<CallToolResponse, ErrorData>(CallToolResult::success(vec![]).into())
            })
        },
    )
}

#[tokio::test(flavor = "current_thread")]
async fn a_panicking_tool_call_records_outcome_panic() {
    let capture = support::capture("trace").await;
    let h = support::harness_with_route(&[], panicking_tool_route()).await;

    tokio::time::timeout(
        Duration::from_secs(10),
        h.client
            .call_tool(CallToolRequestParams::new("panics_for_testing")),
    )
    .await
    .expect("call_tool should return within the timeout, not hang")
    .expect("a panicking tool should return an in-band error, not a protocol error");

    let captured = capture.finish();
    assert!(
        captured.contains("capture armed"),
        "capture observed nothing — this test cannot prove anything: {captured}"
    );
    assert_eq!(
        captured.matches("tool call finished").count(),
        1,
        "expected exactly one closing tool_call record: {captured}"
    );
    assert!(captured.contains(r#"outcome="panic""#), "{captured}");
    assert!(captured.contains(r#"code="INTERNAL""#), "{captured}");
}

const ARG_MARKER: &str = "distinctive-argument-value-9-3-should-never-be-logged";

/// OB3: the `tool_call` span carries only `tool`/`request_id`. Mocks a
/// response that does **not** echo `ARG_MARKER` back, so the only way it
/// could reach the captured log is via the request itself (rmcp's own
/// TRACE dump, floored by 9.0) or a future regression that adds `?params`
/// (or similar) to the span or its closing event.
#[tokio::test(flavor = "current_thread")]
async fn tool_call_never_carries_an_argument_value() {
    let capture = support::capture("trace").await;
    let h = support::harness(&[("REDMINE_CRM_ENABLED", "true")]).await;
    Mock::given(method("POST"))
        .and(path("/contacts.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "contact": {"id": 1, "first_name": "Ada"}
        })))
        .mount(&h.redmine)
        .await;

    let mut request = CallToolRequestParams::new("manage_contact");
    request.arguments = json!({
        "action": "create",
        "first_name": ARG_MARKER,
        "project_id": "my-project"
    })
    .as_object()
    .cloned();
    h.client
        .call_tool(request)
        .await
        .expect("manage_contact create should succeed");

    capture.assert_no_secrets(&[ARG_MARKER]);
}

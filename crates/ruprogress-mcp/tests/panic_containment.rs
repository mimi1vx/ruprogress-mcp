//! e2e: a tool handler that panics answers the caller with an in-band
//! `INTERNAL` error instead of hanging the request, and the session stays
//! usable for the next call. See `panic_guard.rs`'s doc comment for why a
//! hang, not a process crash, is the pre-fix failure mode this guards
//! against — hence every assertion below runs inside `tokio::time::timeout`.
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

const CALL_TIMEOUT: Duration = Duration::from_secs(10);
const PANIC_MESSAGE: &str = "deliberate test panic payload";

fn panicking_tool_route() -> ToolRoute<RedmineMcp> {
    ToolRoute::new_dyn(
        Tool::new(
            "panics_for_testing",
            "panics unconditionally",
            Arc::default(),
        ),
        |_ctx| {
            Box::pin(async {
                panic!("{PANIC_MESSAGE}");
                #[allow(unreachable_code)]
                Ok::<CallToolResponse, ErrorData>(CallToolResult::success(vec![]).into())
            })
        },
    )
}

#[tokio::test]
async fn panicking_tool_returns_internal_error_without_leaking_the_payload() {
    let h = support::harness_with_route(&[], panicking_tool_route()).await;

    let result = tokio::time::timeout(
        CALL_TIMEOUT,
        h.client
            .call_tool(CallToolRequestParams::new("panics_for_testing")),
    )
    .await
    .expect("call_tool should return within the timeout, not hang")
    .expect("a panicking tool should return an in-band error, not a protocol error");

    assert_eq!(result.is_error, Some(true));
    let payload = serde_json::to_string(&result).expect("result should serialize");
    assert!(
        !payload.contains(PANIC_MESSAGE),
        "panic payload leaked into the response: {payload}"
    );

    let structured = result
        .structured_content
        .expect("error result should carry structured_content");
    assert_eq!(structured["code"], "INTERNAL");
    assert_eq!(structured["retryable"], false);

    // The session survives: a real tool call afterwards still works.
    support::mock_current_user(&h.redmine, Some(1)).await;
    let follow_up = tokio::time::timeout(
        CALL_TIMEOUT,
        h.client
            .call_tool(CallToolRequestParams::new("get_current_user")),
    )
    .await
    .expect("follow-up call should return within the timeout")
    .expect("follow-up call should succeed after the panic");
    assert_ne!(follow_up.is_error, Some(true));
}

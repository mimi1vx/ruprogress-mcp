//! Containment for a panicking tool handler: `server.rs`'s `call_tool`
//! wraps its whole body in [`catch_tool_panic`] so one bad handler answers
//! the caller with an in-band error instead of leaving the request hanging
//! (rmcp drops the per-request `JoinHandle`, so an uncaught panic here would
//! otherwise never reach the client at all).

use std::future::Future;
use std::panic::AssertUnwindSafe;

use futures_util::FutureExt as _;
use rmcp::ErrorData as McpError;
use rmcp::model::CallToolResponse;

use crate::tools::output::{self, ErrorCode};

/// Run `fut` (the rest of `call_tool` for `tool`), converting a panic into
/// the same in-band error envelope every other tool failure uses.
///
/// # SAFETY (`AssertUnwindSafe`)
///
/// The state reachable from a panic mid-call is: an immutable `Config`, a
/// `reqwest` connection pool, two `tokio::sync::Mutex`es
/// (`attachments.rs`/`health.rs` — no poisoning, the guard is simply
/// released on unwind), and one `std::sync::Mutex` (`auth/oauth.rs`) whose
/// every `lock()` call already recovers with `PoisonError::into_inner`. None
/// of that can be left in a state this assertion doesn't already account
/// for; re-check this list before adding a new shared mutable field to
/// `RedmineMcp`.
pub(crate) async fn catch_tool_panic<F>(tool: &str, fut: F) -> Result<CallToolResponse, McpError>
where
    F: Future<Output = Result<CallToolResponse, McpError>>,
{
    match AssertUnwindSafe(fut).catch_unwind().await {
        Ok(result) => result,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            tracing::error!(tool = %tool, panic = %message, "tool handler panicked");
            Ok(output::err(
                ErrorCode::Internal,
                "the server encountered an internal error handling this request",
                Some("this is a bug in the server, not in your arguments; do not retry with different arguments"),
            )
            .into())
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use rmcp::model::{CallToolResult, ContentBlock};

    use super::*;

    #[tokio::test]
    async fn passes_through_ok() {
        let fut = async { Ok(CallToolResult::success(vec![]).into()) };
        let result = catch_tool_panic("some_tool", fut)
            .await
            .expect("should not error");
        let CallToolResponse::Complete(result) = result else {
            panic!("expected complete result");
        };
        assert_ne!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn passes_through_err() {
        let fut = async { Err(McpError::invalid_params("bad params", None)) };
        let err = catch_tool_panic("some_tool", fut)
            .await
            .expect_err("should propagate the McpError");
        assert_eq!(err.message, "bad params");
    }

    async fn assert_internal_error(
        fut: impl Future<Output = Result<CallToolResponse, McpError>>,
        forbidden_text: &str,
    ) {
        let result = catch_tool_panic("panicking_tool", fut)
            .await
            .expect("panic should be converted to an Ok result, not propagated");
        let CallToolResponse::Complete(result) = result else {
            panic!("expected complete result");
        };
        assert_eq!(result.is_error, Some(true));
        let structured = result
            .structured_content
            .expect("error result should carry structured_content");
        assert_eq!(structured["code"], "INTERNAL");
        assert_eq!(structured["retryable"], false);
        let text = structured.to_string();
        assert!(
            !text.contains(forbidden_text),
            "panic payload {forbidden_text:?} leaked into the response: {text}"
        );
        let ContentBlock::Text(content_text) = result
            .content
            .first()
            .expect("error result should carry a text content block")
        else {
            panic!("expected a text content block");
        };
        assert!(!content_text.text.contains(forbidden_text));
    }

    #[tokio::test]
    async fn catches_literal_string_panic() {
        let fut = async {
            panic!("literal panic payload");
            #[allow(unreachable_code)]
            Ok(CallToolResult::success(vec![]).into())
        };
        assert_internal_error(fut, "literal panic payload").await;
    }

    #[tokio::test]
    async fn catches_formatted_string_panic() {
        let x = 42;
        let fut = async {
            panic!("formatted panic payload {x}");
            #[allow(unreachable_code)]
            Ok(CallToolResult::success(vec![]).into())
        };
        assert_internal_error(fut, "formatted panic payload 42").await;
    }

    #[tokio::test]
    async fn catches_non_string_panic_payload() {
        let fut = async {
            std::panic::panic_any(42u32);
            #[allow(unreachable_code)]
            Ok(CallToolResult::success(vec![]).into())
        };
        assert_internal_error(fut, "42").await;
    }
}

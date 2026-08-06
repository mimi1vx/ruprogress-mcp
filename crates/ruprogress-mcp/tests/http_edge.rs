//! Edge behaviour of the HTTP transport, asserted with raw `reqwest` rather
//! than an MCP client, because the whole point is what happens *before* the
//! MCP layer ever sees the request.
//!
//! These go through `transport::http::router`, so `nest_service`, the tower
//! layers, and rmcp's own checks are all in the path — which is exactly where
//! a dropped `Host` header would hide.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use reqwest::StatusCode;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .expect("build a test HTTP client")
}

/// A syntactically valid `initialize` POST, so nothing is rejected for the
/// wrong reason.
fn initialize_body() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "edge-test", "version": "0" }
        }
    })
}

#[tokio::test]
async fn oversized_body_is_rejected_with_413() {
    let harness = support::http_harness(&[("REDMINE_MCP_MAX_REQUEST_BODY_BYTES", "1024")]).await;
    let response = client()
        .post(harness.mcp_url())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body("x".repeat(8192))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn a_disallowed_host_header_is_rejected_with_403() {
    let harness = support::http_harness(&[]).await;
    let response = client()
        .post(harness.mcp_url())
        .header("host", "evil.example.com")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&initialize_body())
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_malformed_host_header_is_rejected_with_400() {
    let harness = support::http_harness(&[]).await;
    let response = client()
        .post(harness.mcp_url())
        .header("host", "not a host")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&initialize_body())
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_disallowed_origin_is_rejected_with_403_when_an_allowlist_is_configured() {
    let harness =
        support::http_harness(&[("REDMINE_MCP_ALLOWED_ORIGINS", "https://app.example.com")]).await;
    let response = client()
        .post(harness.mcp_url())
        .header("origin", "https://evil.example.com")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&initialize_body())
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn get_and_delete_on_the_mcp_route_are_405_with_an_allow_header() {
    let harness = support::http_harness(&[]).await;
    for request in [
        client().get(harness.mcp_url()),
        client().delete(harness.mcp_url()),
    ] {
        let response = request
            .header("accept", "text/event-stream")
            .send()
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response
                .headers()
                .get("allow")
                .and_then(|v| v.to_str().ok()),
            Some("POST"),
            "stateless mode serves POST only"
        );
    }
}

#[tokio::test]
async fn a_cors_preflight_from_a_disallowed_origin_gets_no_allow_origin_header() {
    let harness =
        support::http_harness(&[("REDMINE_MCP_ALLOWED_ORIGINS", "https://app.example.com")]).await;
    let response = client()
        .request(reqwest::Method::OPTIONS, harness.mcp_url())
        .header("origin", "https://evil.example.com")
        .header("access-control-request-method", "POST")
        .send()
        .await
        .expect("request should complete");
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
}

#[tokio::test]
async fn a_cors_preflight_from_an_allowed_origin_echoes_that_exact_origin() {
    let harness =
        support::http_harness(&[("REDMINE_MCP_ALLOWED_ORIGINS", "https://app.example.com")]).await;
    let response = client()
        .request(reqwest::Method::OPTIONS, harness.mcp_url())
        .header("origin", "https://app.example.com")
        .header("access-control-request-method", "POST")
        .send()
        .await
        .expect("request should complete");

    let headers = response.headers();
    assert_eq!(
        headers
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("https://app.example.com")
    );
    // A credential-bearing server must never widen this to a wildcard or hand
    // out cookies cross-origin.
    assert!(headers.get("access-control-allow-credentials").is_none());
    let vary = headers
        .get_all("vary")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join(",");
    assert!(vary.to_ascii_lowercase().contains("origin"), "vary: {vary}");
}

#[tokio::test]
async fn no_cors_headers_are_emitted_when_no_origins_are_configured() {
    let harness = support::http_harness(&[]).await;
    let response = client()
        .request(reqwest::Method::OPTIONS, harness.mcp_url())
        .header("origin", "https://app.example.com")
        .header("access-control-request-method", "POST")
        .send()
        .await
        .expect("request should complete");
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
}

#[tokio::test]
async fn an_unknown_path_is_404() {
    let harness = support::http_harness(&[]).await;
    let response = client()
        .get(harness.url("/nope"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn every_response_carries_nosniff() {
    let harness = support::http_harness(&[]).await;
    for url in [
        harness.mcp_url(),
        harness.url("/livez"),
        harness.url("/nope"),
    ] {
        let response = client()
            .get(&url)
            .send()
            .await
            .expect("request should complete");
        assert_eq!(
            response
                .headers()
                .get("x-content-type-options")
                .and_then(|v| v.to_str().ok()),
            Some("nosniff"),
            "missing on {url}"
        );
    }
}

#[tokio::test]
async fn a_custom_mcp_path_is_served_and_the_default_is_not() {
    let harness = support::http_harness(&[("FASTMCP_STREAMABLE_HTTP_PATH", "/api/mcp")]).await;
    let moved = client()
        .get(harness.url("/api/mcp"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(moved.status(), StatusCode::METHOD_NOT_ALLOWED);

    let default = client()
        .get(harness.url("/mcp"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(default.status(), StatusCode::NOT_FOUND);
}

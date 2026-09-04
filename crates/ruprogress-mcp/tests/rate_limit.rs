//! Rate limiting: burst rejection with `Retry-After`, the health exemption,
//! per-key isolation, the strict/standard class split, and the
//! `REDMINE_MCP_RATE_LIMIT_ENABLED=false` escape hatch.
//!
//! Goes through `transport::http::router`, so the real middleware stack —
//! including `ConnectInfo` wiring — is exercised, not a hand-built request.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::sync::OnceLock;

use reqwest::StatusCode;

/// One client for the whole binary: building a fresh `reqwest::Client` per
/// request can cost more than the limiter's refill interval on a loaded
/// runner, which would give the bucket time to recover mid-"burst".
fn client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .build()
                .expect("build a test HTTP client")
        })
        .clone()
}

fn initialize_body() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "rate-limit-test", "version": "0" }
        }
    })
}

async fn call_mcp(harness: &support::HttpHarness, token: Option<&str>) -> reqwest::Response {
    let mut request = client()
        .post(harness.mcp_url())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&initialize_body());
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    request.send().await.expect("request should complete")
}

fn oauth_proxy_env(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut env = vec![
        ("REDMINE_AUTH_MODE", "oauth-proxy"),
        ("REDMINE_MCP_BASE_URL", "http://localhost:3040"),
        ("REDMINE_INTROSPECT_CLIENT_ID", "introspect-client"),
        ("REDMINE_INTROSPECT_CLIENT_SECRET", "introspect-secret"),
    ];
    env.extend_from_slice(extra);
    env
}

#[tokio::test]
async fn a_burst_to_mcp_gets_429_with_retry_after_while_livez_stays_up() {
    // Refill pinned to 1/s, capacity left at its default 40: at the default
    // 10/s a 100-request loop only ever empties the bucket if every request
    // completes within 60ms, so a slow runner refills faster than the loop
    // drains and the burst never trips.
    let harness = support::http_harness(&[("REDMINE_MCP_RATE_LIMIT_RPS", "1")]).await;

    let mut saw_429 = false;
    for _ in 0..100 {
        let response = call_mcp(&harness, None).await;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;
            assert!(
                response.headers().get("retry-after").is_some(),
                "a 429 must carry Retry-After"
            );
            assert_eq!(
                response
                    .headers()
                    .get("cache-control")
                    .and_then(|v| v.to_str().ok()),
                Some("no-store")
            );
            let body: serde_json::Value = response.json().await.expect("json body");
            assert_eq!(body["error"], "rate_limited");
            break;
        }
    }
    assert!(
        saw_429,
        "a 100-request burst should trip the standard class"
    );

    let livez = client()
        .get(harness.url("/livez"))
        .send()
        .await
        .expect("request should complete");
    assert_eq!(
        livez.status(),
        StatusCode::OK,
        "/livez must never be rate limited (RL6)"
    );
}

#[tokio::test]
async fn register_trips_an_order_of_magnitude_sooner_than_mcp() {
    // Defaults: standard burst 40, strict burst 10 — 15 requests trips the
    // strict class but not the standard one.
    let harness = support::http_harness(&oauth_proxy_env(&[])).await;

    let mut register_saw_429 = false;
    for _ in 0..15 {
        let response = client()
            .post(harness.url("/register"))
            .header("content-type", "application/json")
            .json(&serde_json::json!({ "redirect_uris": ["http://localhost/cb"] }))
            .send()
            .await
            .expect("request should complete");
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            register_saw_429 = true;
            break;
        }
    }
    assert!(
        register_saw_429,
        "the strict class's default burst (10) should trip within 15 requests"
    );

    let mut mcp_saw_429 = false;
    for _ in 0..15 {
        if call_mcp(&harness, None).await.status() == StatusCode::TOO_MANY_REQUESTS {
            mcp_saw_429 = true;
            break;
        }
    }
    assert!(
        !mcp_saw_429,
        "the standard class's default burst (40) should not trip within 15 requests"
    );
}

#[tokio::test]
async fn distinct_bearer_tokens_from_the_same_ip_do_not_share_a_bucket() {
    let harness = support::http_harness(&[
        ("REDMINE_MCP_RATE_LIMIT_RPS", "1"),
        ("REDMINE_MCP_RATE_LIMIT_BURST", "1"),
    ])
    .await;

    assert_ne!(
        call_mcp(&harness, Some("token-a")).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "first request on a fresh bucket should be allowed"
    );
    assert_eq!(
        call_mcp(&harness, Some("token-a")).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the same token's bucket (burst 1) should now be exhausted"
    );
    assert_ne!(
        call_mcp(&harness, Some("token-b")).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "a distinct token from the same IP must get its own bucket"
    );
}

#[tokio::test]
async fn two_anonymous_callers_from_the_same_ip_share_a_bucket() {
    let harness = support::http_harness(&[
        ("REDMINE_MCP_RATE_LIMIT_RPS", "1"),
        ("REDMINE_MCP_RATE_LIMIT_BURST", "1"),
    ])
    .await;

    assert_ne!(
        call_mcp(&harness, None).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        call_mcp(&harness, None).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "no Authorization header on either call: both key by the same peer IP"
    );
}

#[tokio::test]
async fn rate_limit_enabled_false_never_rejects() {
    let harness = support::http_harness(&[("REDMINE_MCP_RATE_LIMIT_ENABLED", "false")]).await;

    for _ in 0..100 {
        assert_ne!(
            call_mcp(&harness, None).await.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "REDMINE_MCP_RATE_LIMIT_ENABLED=false must restore pre-9.2 behaviour exactly"
        );
    }
}

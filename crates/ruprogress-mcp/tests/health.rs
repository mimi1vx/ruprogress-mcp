//! `/livez`, `/readyz`, and the `/health` alias.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use reqwest::StatusCode;
use serde_json::Value;
use wiremock::matchers::{method, path};

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .expect("build a test HTTP client")
}

async fn get(url: &str) -> reqwest::Response {
    client()
        .get(url)
        .send()
        .await
        .expect("request should complete")
}

async fn mock_failing_redmine(redmine: &wiremock::MockServer) {
    wiremock::Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .respond_with(wiremock::ResponseTemplate::new(500))
        .mount(redmine)
        .await;
}

#[tokio::test]
async fn livez_is_200_even_when_redmine_is_down() {
    let harness = support::http_harness(&[]).await;
    // No mock mounted at all: wiremock answers every request with 404, and
    // /livez must not care.
    let response = get(&harness.url("/livez")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["status"], "alive");
}

#[tokio::test]
async fn readyz_is_200_when_redmine_answers() {
    let harness = support::http_harness(&[]).await;
    support::mock_current_user(&harness.redmine, None).await;

    let response = get(&harness.url("/readyz")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["status"], "ready");
    assert_eq!(body["redmine"], "up");
}

#[tokio::test]
async fn readyz_is_503_when_redmine_errors() {
    let harness = support::http_harness(&[]).await;
    mock_failing_redmine(&harness.redmine).await;

    let response = get(&harness.url("/readyz")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["status"], "not_ready");
    assert_eq!(body["redmine"], "down");
}

#[tokio::test]
async fn readyz_is_503_when_redmine_is_unreachable() {
    // A port nothing is listening on: the transport-error path, distinct from
    // the HTTP-error path above.
    let closed = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind to find a free port");
    let port = closed.local_addr().expect("local addr").port();
    drop(closed);

    let harness =
        support::http_harness(&[("REDMINE_URL", &format!("http://127.0.0.1:{port}"))]).await;
    let response = get(&harness.url("/readyz")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn the_health_alias_maps_to_readyz() {
    let harness = support::http_harness(&[]).await;
    support::mock_current_user(&harness.redmine, None).await;

    let readyz: Value = get(&harness.url("/readyz"))
        .await
        .json()
        .await
        .expect("json body");
    let health: Value = get(&harness.url("/health"))
        .await
        .json()
        .await
        .expect("json body");

    let keys = |v: &Value| {
        let mut k: Vec<String> = v
            .as_object()
            .expect("object body")
            .keys()
            .cloned()
            .collect();
        k.sort();
        k
    };
    assert_eq!(keys(&readyz), keys(&health));
    assert_eq!(readyz["redmine"], health["redmine"]);
}

#[tokio::test]
async fn concurrent_readyz_requests_collapse_into_one_upstream_probe() {
    let harness = support::http_harness(&[]).await;
    // The delay is what makes this a test of the *lock* rather than of the TTL
    // cache: without it the five requests could complete one after another,
    // and every one after the first would hit a warm cache no matter where the
    // probe runs.
    wiremock::Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(400))
                .set_body_json(serde_json::json!({
                    "user": {
                        "id": 5, "login": "alice", "firstname": "Alice",
                        "lastname": "Example", "mail": "alice@example.com",
                        "created_on": "2024-01-01T00:00:00Z",
                        "last_login_on": "2026-08-01T00:00:00Z",
                    }
                })),
        )
        .expect(1)
        .mount(&harness.redmine)
        .await;

    // Spawned, not awaited in sequence: the probes have to actually overlap
    // for the collapsing to be under test at all.
    let mut probes = tokio::task::JoinSet::new();
    for _ in 0..5 {
        let url = harness.url("/readyz");
        probes.spawn(async move { get(&url).await.status() });
    }
    while let Some(joined) = probes.join_next().await {
        assert_eq!(joined.expect("probe task should not panic"), StatusCode::OK);
    }
    // Dropping the harness verifies wiremock's `expect(1)`: five requests in,
    // one upstream call out.
}

#[tokio::test]
async fn readyz_reports_not_probed_when_the_server_owns_no_credential() {
    let harness = support::http_harness(&[
        ("REDMINE_AUTH_MODE", "oauth"),
        ("REDMINE_MCP_BASE_URL", "http://localhost:3040"),
    ])
    .await;

    let response = get(&harness.url("/readyz")).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "no server-owned credential is not a reason to leave the rotation"
    );
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["status"], "ready");
    assert_eq!(body["redmine"], "not_probed");
}

#[tokio::test]
async fn the_readyz_body_carries_exactly_three_readiness_keys_and_no_config() {
    let harness = support::http_harness(&[]).await;
    support::mock_current_user(&harness.redmine, None).await;

    let response = get(&harness.url("/readyz")).await;
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let raw = response.text().await.expect("text body");
    let body: Value = serde_json::from_str(&raw).expect("json body");

    let mut keys: Vec<&String> = body.as_object().expect("object body").keys().collect();
    keys.sort();
    // An exact key set, not a substring scan: a future field cannot be added
    // to this unauthenticated endpoint without this test failing.
    assert_eq!(keys, ["checked_at", "redmine", "status"]);

    assert!(!raw.contains("test-api-key"), "{raw}");
    assert!(!raw.contains(&harness.redmine.uri()), "{raw}");
    assert!(!raw.contains("127.0.0.1"), "{raw}");
}

#[tokio::test]
async fn all_three_endpoints_send_no_store() {
    let harness = support::http_harness(&[]).await;
    support::mock_current_user(&harness.redmine, None).await;
    for path in ["/livez", "/readyz", "/health"] {
        let response = get(&harness.url(path)).await;
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-store"),
            "missing on {path}"
        );
    }
}

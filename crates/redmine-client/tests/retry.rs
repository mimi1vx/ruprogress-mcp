//! End-to-end retry behavior: GET retries transient failures, POST never
//! does, `Retry-After` is honoured and clamped, and the whole retry budget
//! stays within the configured timeout.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::time::Instant;

use redmine_client::model::issue::IssueCreate;
use redmine_client::{Credential, IssueId, ProjectIdent};
use secrecy::SecretString;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

fn issue_body() -> serde_json::Value {
    serde_json::json!({
        "issue": {
            "id": 1,
            "project": {"id": 1, "name": "P"},
            "tracker": {"id": 1, "name": "Bug"},
            "status": {"id": 1, "name": "New"},
            "priority": {"id": 1, "name": "Normal"},
            "author": {"id": 1, "name": "A"},
            "subject": "s",
            "created_on": "2026-01-01T00:00:00Z",
            "updated_on": "2026-01-01T00:00:00Z"
        }
    })
}

#[tokio::test]
async fn get_retries_503_then_succeeds() {
    let (server, client) = support::mock_redmine_fast().await;
    // First two attempts fail with 503, third succeeds: proves the retry
    // path is exercised (not that it's exhausted).
    Mock::given(method("GET"))
        .and(path("/issues/1.json"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_body()))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let issue = client
        .as_user(&cred)
        .get_issue(IssueId(1), &[])
        .await
        .expect("GET should retry past transient 503s and eventually succeed");
    assert_eq!(issue.id, 1);
}

#[tokio::test]
async fn post_never_retries_a_503() {
    let (server, client) = support::mock_redmine_fast().await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1) // exactly one attempt: proves POST is never retried
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let project = "demo".parse().unwrap();
    let create = IssueCreate::new(ProjectIdent::Identifier(project), "x");
    let err = client
        .as_user(&cred)
        .create_issue(&create)
        .await
        .expect_err("a 503 must surface as an error, not be retried");
    match err {
        redmine_client::Error::Api { status, .. } => {
            assert_eq!(status, http::StatusCode::SERVICE_UNAVAILABLE);
        }
        other => panic!("expected Api(503), got {other:?}"),
    }
}

#[tokio::test]
async fn retry_after_seconds_is_honoured() {
    let (server, client) = support::mock_redmine_fast().await;
    Mock::given(method("GET"))
        .and(path("/issues/1.json"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_body()))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let issue = client
        .as_user(&cred)
        .get_issue(IssueId(1), &[])
        .await
        .expect("429 with Retry-After should be retried and succeed");
    assert_eq!(issue.id, 1);
}

#[tokio::test]
async fn hostile_retry_after_is_clamped_and_elapsed_stays_within_timeout() {
    let (server, client) = support::mock_redmine_fast().await;
    // mock_redmine_fast's client timeout is 800ms and max_backoff is 50ms;
    // a server demanding a day-long Retry-After must not be honoured
    // literally.
    Mock::given(method("GET"))
        .and(path("/issues/1.json"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "86400"))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let start = Instant::now();
    let err = client
        .as_user(&cred)
        .get_issue(IssueId(1), &[])
        .await
        .expect_err("a persistent 429 exhausts retries and surfaces as an error");
    let elapsed = start.elapsed();

    assert!(matches!(err, redmine_client::Error::RateLimited { .. }));
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "hostile Retry-After must be clamped, not honoured literally: took {elapsed:?}"
    );
}

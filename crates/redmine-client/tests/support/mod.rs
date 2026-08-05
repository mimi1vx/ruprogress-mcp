//! Shared harness for `redmine-client` integration tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use redmine_client::{Credential, RedmineClient, RedmineClientBuilder};
use secrecy::SecretString;

/// Start a `wiremock` server and a [`RedmineClient`] pointed at it, with a
/// test API key as its default credential.
pub(crate) async fn mock_redmine() -> (wiremock::MockServer, RedmineClient) {
    let server = wiremock::MockServer::start().await;
    let base = server
        .uri()
        .parse()
        .expect("mock server URI should parse as a URL");
    let client = RedmineClientBuilder::new(base)
        .credential(Credential::ApiKey(SecretString::from("test-api-key")))
        .build()
        .expect("client should build against a valid base URL");
    (server, client)
}

/// Like [`mock_redmine`], but with the client's total timeout and retry
/// backoff shrunk so retry/timeout tests run fast.
pub(crate) async fn mock_redmine_fast() -> (wiremock::MockServer, RedmineClient) {
    let server = wiremock::MockServer::start().await;
    let base = server
        .uri()
        .parse()
        .expect("mock server URI should parse as a URL");
    let client = RedmineClientBuilder::new(base)
        .credential(Credential::ApiKey(SecretString::from("test-api-key")))
        .timeout(std::time::Duration::from_millis(800))
        .retry_policy(redmine_client::RetryPolicy {
            max_retries: 3,
            base: std::time::Duration::from_millis(10),
            max_backoff: std::time::Duration::from_millis(50),
        })
        .build()
        .expect("client should build against a valid base URL");
    (server, client)
}

/// Read `tests/fixtures/{name}.json`.
///
/// # Panics
///
/// Panics if the fixture file is missing.
pub(crate) fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("missing fixture {name}: {e}"))
}

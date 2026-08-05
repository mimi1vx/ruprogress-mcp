//! End-to-end proof that a base URL with a sub-path (`/redmine/`) reaches
//! the right endpoint. Direct rejection tests for hostile paths live as unit
//! tests next to `Scoped::endpoint` in `src/client.rs`, since it is a
//! private method.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use redmine_client::{Credential, IssueId, RedmineClientBuilder};
use secrecy::SecretString;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn base_url_with_sub_path_reaches_the_correct_endpoint() {
    let server = MockServer::start().await;
    // Deployments behind a sub-path (e.g. reverse-proxied at /redmine/) are
    // common; the base's own path segment must survive `endpoint()`.
    let base = format!("{}/redmine/", server.uri()).parse().unwrap();

    Mock::given(method("GET"))
        .and(path("/redmine/issues/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
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
        })))
        .mount(&server)
        .await;

    let client = RedmineClientBuilder::new(base)
        .credential(Credential::ApiKey(SecretString::from("k")))
        .build()
        .unwrap();
    let cred = Credential::ApiKey(SecretString::from("k"));
    let issue = client
        .as_user(&cred)
        .get_issue(IssueId(1), &[])
        .await
        .expect("request should reach /redmine/issues/1.json, not /issues/1.json");
    assert_eq!(issue.id, 1);
}

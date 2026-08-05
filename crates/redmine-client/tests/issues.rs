//! Happy-path and dominant-error-path tests for the issue endpoints.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use redmine_client::model::issue::{IssueCreate, IssueUpdate};
use redmine_client::{Credential, Error, IssueId, ProjectIdent};
use secrecy::SecretString;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, ResponseTemplate};

fn issue_json(id: u64, subject: &str) -> serde_json::Value {
    serde_json::json!({
        "issue": {
            "id": id,
            "project": {"id": 1, "name": "P"},
            "tracker": {"id": 1, "name": "Bug"},
            "status": {"id": 1, "name": "New"},
            "priority": {"id": 1, "name": "Normal"},
            "author": {"id": 1, "name": "A"},
            "subject": subject,
            "created_on": "2026-01-01T00:00:00Z",
            "updated_on": "2026-01-01T00:00:00Z"
        }
    })
}

#[tokio::test]
async fn get_issue_happy_path() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/issues/42.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(42, "Fix the bug")))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let issue = client
        .as_user(&cred)
        .get_issue(IssueId(42), &[])
        .await
        .unwrap();
    assert_eq!(issue.id, 42);
    assert_eq!(issue.subject, "Fix the bug");
}

#[tokio::test]
async fn get_issue_not_found() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/issues/999.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let err = client
        .as_user(&cred)
        .get_issue(IssueId(999), &[])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound));
}

#[tokio::test]
async fn create_issue_happy_path_sends_expected_body() {
    let (server, client) = support::mock_redmine().await;
    let project: redmine_client::ProjectIdentifier = "demo".parse().unwrap();
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .and(body_json(serde_json::json!({
            "issue": { "project_id": "demo", "subject": "New issue" }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(1, "New issue")))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let create = IssueCreate::new(ProjectIdent::Identifier(project), "New issue");
    let issue = client.as_user(&cred).create_issue(&create).await.unwrap();
    assert_eq!(issue.subject, "New issue");
}

#[tokio::test]
async fn update_issue_happy_path() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let patch = IssueUpdate {
        notes: Some("done".to_string()),
        ..Default::default()
    };
    client
        .as_user(&cred)
        .update_issue(IssueId(7), &patch)
        .await
        .unwrap();
}

#[tokio::test]
async fn update_issue_dominant_error_422() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("PUT"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
            "errors": ["Status is invalid"]
        })))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let patch = IssueUpdate {
        status_id: Some(9999),
        ..Default::default()
    };
    let err = client
        .as_user(&cred)
        .update_issue(IssueId(7), &patch)
        .await
        .unwrap_err();
    match err {
        Error::Api { errors, .. } => assert_eq!(errors, vec!["Status is invalid".to_string()]),
        other => panic!("expected Api, got {other:?}"),
    }
}

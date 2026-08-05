//! Happy-path and dominant-error-path tests for the time-entry endpoints.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use redmine_client::model::time_entry::TimeEntryCreate;
use redmine_client::{Credential, Error, IssueId};
use secrecy::SecretString;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

fn time_entry_json(id: u64, hours: f64) -> serde_json::Value {
    serde_json::json!({
        "time_entry": {
            "id": id,
            "project": {"id": 1, "name": "P"},
            "issue": {"id": 5},
            "user": {"id": 2, "name": "Bob"},
            "activity": {"id": 9, "name": "Development"},
            "hours": hours,
            "spent_on": "2026-01-05",
            "created_on": "2026-01-05T00:00:00Z",
            "updated_on": "2026-01-05T00:00:00Z"
        }
    })
}

#[tokio::test]
async fn create_time_entry_happy_path() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/time_entries.json"))
        .respond_with(ResponseTemplate::new(201).set_body_json(time_entry_json(1, 2.5)))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let create = TimeEntryCreate::for_issue(IssueId(5), 2.5);
    let entry = client
        .as_user(&cred)
        .create_time_entry(&create)
        .await
        .unwrap();
    assert!((entry.hours - 2.5).abs() < f64::EPSILON);
    assert_eq!(entry.issue.map(|i| i.id), Some(5));
}

#[tokio::test]
async fn create_time_entry_dominant_error_422() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/time_entries.json"))
        .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
            "errors": ["Hours can't be blank"]
        })))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let create = TimeEntryCreate::for_issue(IssueId(5), 0.0);
    let err = client
        .as_user(&cred)
        .create_time_entry(&create)
        .await
        .unwrap_err();
    match err {
        Error::Api { errors, .. } => assert_eq!(errors, vec!["Hours can't be blank".to_string()]),
        other => panic!("expected Api, got {other:?}"),
    }
}

#[tokio::test]
async fn list_time_entries_unauthorized() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/time_entries.json"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let err = client
        .as_user(&cred)
        .list_time_entries(&redmine_client::model::time_entry::TimeEntryQuery::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unauthorized));
}

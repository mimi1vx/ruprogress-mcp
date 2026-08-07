//! Happy-path and dominant-error-path tests for the issue endpoints.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use redmine_client::model::issue::{IssueCreate, IssueUpdate};
use redmine_client::model::issue_category::{IssueCategoryCreate, IssueCategoryUpdate};
use redmine_client::model::journal::JournalUpdate;
use redmine_client::model::relation::IssueRelationCreate;
use redmine_client::{
    Credential, Error, IssueCategoryId, IssueId, JournalId, ProjectIdent, RelationId, UserId,
};
use secrecy::SecretString;
use wiremock::matchers::{body_json, method, path, query_param};
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
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7, "Fixed subject")))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let patch = IssueUpdate {
        notes: Some("done".to_string()),
        ..Default::default()
    };
    let issue = client
        .as_user(&cred)
        .update_issue(IssueId(7), &patch)
        .await
        .unwrap();
    assert_eq!(issue.id, 7);
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

// --- 4b-write ---

#[tokio::test]
async fn delete_issue_succeeds_on_204() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("DELETE"))
        .and(path("/issues/7.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    client
        .as_user(&cred)
        .delete_issue(IssueId(7))
        .await
        .unwrap();
}

#[tokio::test]
async fn delete_issue_not_found() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("DELETE"))
        .and(path("/issues/999.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let err = client
        .as_user(&cred)
        .delete_issue(IssueId(999))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound));
}

#[tokio::test]
async fn list_relations_sends_no_pagination_params_and_parses_a_bare_array() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/issues/9/relations.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "relations": [
                {"id": 1, "issue_id": 9, "issue_to_id": 7, "relation_type": "relates", "delay": null}
            ]
        })))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let relations = client
        .as_user(&cred)
        .list_relations(IssueId(9))
        .await
        .unwrap();
    assert_eq!(relations.len(), 1);
    assert_eq!(relations[0].issue_to_id, 7);
}

#[tokio::test]
async fn create_relation_sends_expected_body() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/issues/9/relations.json"))
        .and(body_json(serde_json::json!({
            "relation": {"issue_to_id": 7, "relation_type": "blocks"}
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "relation": {"id": 1, "issue_id": 9, "issue_to_id": 7, "relation_type": "blocks", "delay": null}
        })))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let new = IssueRelationCreate {
        issue_to_id: IssueId(7),
        relation_type: Some("blocks".to_string()),
        delay: None,
    };
    let relation = client
        .as_user(&cred)
        .create_relation(IssueId(9), &new)
        .await
        .unwrap();
    assert_eq!(relation.id, 1);
}

#[tokio::test]
async fn create_relation_dominant_error_422_same_project_violation() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/issues/9/relations.json"))
        .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
            "errors": ["Issue to id is not in the same project"]
        })))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let new = IssueRelationCreate {
        issue_to_id: IssueId(7),
        relation_type: None,
        delay: None,
    };
    let err = client
        .as_user(&cred)
        .create_relation(IssueId(9), &new)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Api { .. }));
}

#[tokio::test]
async fn delete_relation_succeeds_on_204() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("DELETE"))
        .and(path("/relations/1.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    client
        .as_user(&cred)
        .delete_relation(RelationId(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn add_watcher_sends_user_id_body() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/issues/9/watchers.json"))
        .and(body_json(serde_json::json!({"user_id": 3})))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    client
        .as_user(&cred)
        .add_watcher(IssueId(9), UserId(3))
        .await
        .unwrap();
}

#[tokio::test]
async fn remove_watcher_succeeds_on_204() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("DELETE"))
        .and(path("/issues/9/watchers/3.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    client
        .as_user(&cred)
        .remove_watcher(IssueId(9), UserId(3))
        .await
        .unwrap();
}

#[tokio::test]
async fn update_journal_sends_expected_body_and_no_follow_up_get() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("PUT"))
        .and(path("/journals/5.json"))
        .and(body_json(serde_json::json!({
            "journal": {"notes": "edited", "private_notes": true}
        })))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let patch = JournalUpdate {
        notes: Some("edited".to_string()),
        private_notes: Some(true),
    };
    client
        .as_user(&cred)
        .update_journal(JournalId(5), &patch)
        .await
        .unwrap();
}

#[tokio::test]
async fn update_journal_forbidden() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("PUT"))
        .and(path("/journals/5.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let patch = JournalUpdate {
        notes: Some("edited".to_string()),
        private_notes: None,
    };
    let err = client
        .as_user(&cred)
        .update_journal(JournalId(5), &patch)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Forbidden));
}

#[tokio::test]
async fn list_issue_categories_ignores_total_count() {
    let (server, client) = support::mock_redmine().await;
    let project: redmine_client::ProjectIdentifier = "demo".parse().unwrap();
    Mock::given(method("GET"))
        .and(path("/projects/demo/issue_categories.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issue_categories": [{"id": 2, "name": "Backend"}],
            "total_count": 1
        })))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let categories = client
        .as_user(&cred)
        .list_issue_categories(&ProjectIdent::Identifier(project))
        .await
        .unwrap();
    assert_eq!(categories.len(), 1);
}

#[tokio::test]
async fn create_issue_category_sends_expected_body() {
    let (server, client) = support::mock_redmine().await;
    let project: redmine_client::ProjectIdentifier = "demo".parse().unwrap();
    Mock::given(method("POST"))
        .and(path("/projects/demo/issue_categories.json"))
        .and(body_json(serde_json::json!({
            "issue_category": {"name": "Backend"}
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "issue_category": {"id": 2, "name": "Backend"}
        })))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let new = IssueCategoryCreate {
        name: "Backend".to_string(),
        assigned_to_id: None,
    };
    let category = client
        .as_user(&cred)
        .create_issue_category(&ProjectIdent::Identifier(project), &new)
        .await
        .unwrap();
    assert_eq!(category.id, 2);
}

#[tokio::test]
async fn update_issue_category_issues_a_put_then_exactly_one_get() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("PUT"))
        .and(path("/issue_categories/2.json"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/issue_categories/2.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issue_category": {"id": 2, "name": "Frontend"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let patch = IssueCategoryUpdate {
        name: Some("Frontend".to_string()),
        assigned_to_id: None,
    };
    let category = client
        .as_user(&cred)
        .update_issue_category(IssueCategoryId(2), &patch)
        .await
        .unwrap();
    assert_eq!(category.name, "Frontend");
}

#[tokio::test]
async fn delete_issue_category_sends_reassign_to_id_as_a_top_level_query_param() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("DELETE"))
        .and(path("/issue_categories/2.json"))
        .and(query_param("reassign_to_id", "3"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    client
        .as_user(&cred)
        .delete_issue_category(IssueCategoryId(2), Some(IssueCategoryId(3)))
        .await
        .unwrap();
}

#[tokio::test]
async fn delete_issue_category_without_reassign_sends_no_query_param() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("DELETE"))
        .and(path("/issue_categories/2.json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    client
        .as_user(&cred)
        .delete_issue_category(IssueCategoryId(2), None)
        .await
        .unwrap();
}

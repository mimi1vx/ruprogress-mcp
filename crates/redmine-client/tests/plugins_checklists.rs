//! `RedmineUP` Checklists Pro: `GET/POST /issues/{id}/checklists.json`,
//! `PUT /checklists/{id}.json`. Synthetic fixtures — see
//! `tests/fixtures/README.md`'s plugin fixtures section.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use redmine_client::model::plugins::checklists::{ChecklistItemCreate, ChecklistItemUpdate};
use redmine_client::{ChecklistItemId, Credential, Error, IssueId};
use secrecy::SecretString;
use wiremock::matchers::{body_json, method, path, query_param_is_missing};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test]
async fn list_checklist_items_parses_the_envelope_shape_and_sends_no_pagination_params() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/issues/1/checklists.json"))
        .and(query_param_is_missing("limit"))
        .and(query_param_is_missing("offset"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("checklist_items")),
        )
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let items = client
        .as_user(&cred)
        .list_checklist_items(IssueId(1))
        .await
        .unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].subject, "Write tests");
    assert_eq!(items[1].is_done, Some(true));
}

#[tokio::test]
async fn list_checklist_items_also_parses_the_bare_array_shape() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/issues/1/checklists.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("checklist_items_bare")),
        )
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let items = client
        .as_user(&cred)
        .list_checklist_items(IssueId(1))
        .await
        .unwrap();
    assert_eq!(items.len(), 2);
}

#[tokio::test]
async fn list_checklist_items_on_a_third_shape_is_a_decode_error() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/issues/1/checklists.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "unexpected": true
        })))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let err = client
        .as_user(&cred)
        .list_checklist_items(IssueId(1))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode { .. }));
}

#[tokio::test]
async fn create_checklist_item_sends_expected_body_and_reads_the_nested_id() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/issues/1/checklists.json"))
        .and(body_json(serde_json::json!({
            "checklist": {"subject": "Write tests", "is_section": false, "is_done": false}
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("checklist_item_created")),
        )
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let new = ChecklistItemCreate {
        subject: "Write tests".to_string(),
        is_section: Some(false),
        is_done: Some(false),
        position: None,
    };
    let id = client
        .as_user(&cred)
        .create_checklist_item(IssueId(1), &new)
        .await
        .unwrap();
    assert_eq!(id, Some(ChecklistItemId(3)));
}

#[tokio::test]
async fn create_checklist_item_with_no_id_in_the_response_is_none_not_an_error() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/issues/1/checklists.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let new = ChecklistItemCreate {
        subject: "Write tests".to_string(),
        is_section: None,
        is_done: None,
        position: None,
    };
    let id = client
        .as_user(&cred)
        .create_checklist_item(IssueId(1), &new)
        .await
        .unwrap();
    assert_eq!(id, None);
}

#[tokio::test]
async fn update_checklist_item_sends_expected_body() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("PUT"))
        .and(path("/checklists/5.json"))
        .and(body_json(serde_json::json!({
            "checklist": {"is_done": true}
        })))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let patch = ChecklistItemUpdate {
        subject: None,
        is_done: Some(true),
        position: None,
    };
    client
        .as_user(&cred)
        .update_checklist_item(ChecklistItemId(5), &patch)
        .await
        .unwrap();
}

#[tokio::test]
async fn update_checklist_item_forbidden() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("PUT"))
        .and(path("/checklists/5.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let patch = ChecklistItemUpdate {
        subject: Some("edited".to_string()),
        is_done: None,
        position: None,
    };
    let err = client
        .as_user(&cred)
        .update_checklist_item(ChecklistItemId(5), &patch)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Forbidden));
}

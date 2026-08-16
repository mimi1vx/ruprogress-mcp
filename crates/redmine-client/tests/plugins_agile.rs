//! `RedmineUP` Agile: `GET /issues/{id}/agile_data.json`,
//! `PUT /issues/{id}.json` with a nested `agile_data_attributes`. Synthetic
//! fixtures — see `tests/fixtures/README.md`'s plugin fixtures section.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use redmine_client::model::plugins::agile::AgileDataAttributes;
use redmine_client::{Credential, Error, IssueId};
use secrecy::SecretString;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test]
async fn get_agile_data_parses_a_populated_row() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/issues/1/agile_data.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("agile_data_full")),
        )
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let row = client
        .as_user(&cred)
        .get_agile_data(IssueId(1))
        .await
        .unwrap()
        .expect("row should be present");
    assert_eq!(row.id, Some(9));
    assert_eq!(row.story_points, Some(8));
    assert_eq!(row.agile_sprint_id, Some(3));
    assert_eq!(row.position, Some(2));
}

#[tokio::test]
async fn get_agile_data_on_a_null_row_is_ok_none() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/issues/1/agile_data.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("agile_data_empty")),
        )
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let row = client
        .as_user(&cred)
        .get_agile_data(IssueId(1))
        .await
        .unwrap();
    assert!(row.is_none());
}

#[tokio::test]
async fn get_agile_data_on_a_404_is_ok_none_not_an_error() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/issues/1/agile_data.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let row = client
        .as_user(&cred)
        .get_agile_data(IssueId(1))
        .await
        .unwrap();
    assert!(row.is_none());
}

#[tokio::test]
async fn get_agile_data_forbidden_is_a_real_error() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/issues/1/agile_data.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let err = client
        .as_user(&cred)
        .get_agile_data(IssueId(1))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Forbidden));
}

#[tokio::test]
async fn update_agile_data_sends_the_nested_attributes_shape() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("PUT"))
        .and(path("/issues/1.json"))
        .and(body_json(serde_json::json!({
            "issue": {"agile_data_attributes": {"id": 9, "agile_sprint_id": 7}}
        })))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let attrs = AgileDataAttributes {
        id: Some(9),
        agile_sprint_id: Some(7),
        ..Default::default()
    };
    client
        .as_user(&cred)
        .update_agile_data(IssueId(1), &attrs)
        .await
        .unwrap();
}

#[tokio::test]
async fn update_agile_data_forbidden() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("PUT"))
        .and(path("/issues/1.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let err = client
        .as_user(&cred)
        .update_agile_data(IssueId(1), &AgileDataAttributes::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Forbidden));
}

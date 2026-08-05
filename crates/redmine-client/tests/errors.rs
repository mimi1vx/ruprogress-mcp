//! Status-code -> `Error` mapping, end to end through a real HTTP request.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use redmine_client::Error;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test]
async fn get_json_maps_401() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/issues/1.json"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let cred = redmine_client::Credential::ApiKey(secrecy::SecretString::from("k"));
    let err = client
        .as_user(&cred)
        .get_issue(redmine_client::IssueId(1), &[])
        .await
        .expect_err("401 should be an error");
    assert!(matches!(err, Error::Unauthorized));
}

#[tokio::test]
async fn get_json_maps_404() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/issues/1.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let cred = redmine_client::Credential::ApiKey(secrecy::SecretString::from("k"));
    let err = client
        .as_user(&cred)
        .get_issue(redmine_client::IssueId(1), &[])
        .await
        .expect_err("404 should be an error");
    assert!(matches!(err, Error::NotFound));
}

#[tokio::test]
async fn create_issue_maps_422_with_parsed_errors() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/issues.json"))
        .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
            "errors": ["Subject can't be blank"]
        })))
        .mount(&server)
        .await;

    let cred = redmine_client::Credential::ApiKey(secrecy::SecretString::from("k"));
    let project = "demo".parse().unwrap();
    let create = redmine_client::model::issue::IssueCreate::new(
        redmine_client::ProjectIdent::Identifier(project),
        "x",
    );
    let err = client
        .as_user(&cred)
        .create_issue(&create)
        .await
        .expect_err("422 should be an error");
    match err {
        Error::Api { status, errors } => {
            assert_eq!(status, http::StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(errors, vec!["Subject can't be blank".to_string()]);
        }
        other => panic!("expected Api, got {other:?}"),
    }
}

#[tokio::test]
async fn get_json_maps_500_with_unparseable_body_to_empty_errors() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/issues/1.json"))
        .respond_with(ResponseTemplate::new(500).set_body_string("not json"))
        .mount(&server)
        .await;

    let cred = redmine_client::Credential::ApiKey(secrecy::SecretString::from("k"));
    let err = client
        .as_user(&cred)
        .get_issue(redmine_client::IssueId(1), &[])
        .await
        .expect_err("500 should be an error");
    match err {
        Error::Api { status, errors } => {
            assert_eq!(status, http::StatusCode::INTERNAL_SERVER_ERROR);
            assert!(errors.is_empty());
        }
        other => panic!("expected Api, got {other:?}"),
    }
}

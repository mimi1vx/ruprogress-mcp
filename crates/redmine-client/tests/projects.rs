//! Happy-path and dominant-error-path tests for the project endpoints.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use redmine_client::model::project::{ProjectInclude, ProjectQuery};
use redmine_client::{Credential, Error, ProjectId, ProjectIdent};
use secrecy::SecretString;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

fn project_json(id: u64, identifier: &str) -> serde_json::Value {
    serde_json::json!({
        "project": {
            "id": id,
            "name": "Example",
            "identifier": identifier,
            "created_on": "2026-01-01T00:00:00Z",
            "updated_on": "2026-01-01T00:00:00Z"
        }
    })
}

#[tokio::test]
async fn get_project_happy_path_by_identifier() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/projects/demo.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(project_json(1, "demo")))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let ident = ProjectIdent::Identifier("demo".parse().unwrap());
    let project = client
        .as_user(&cred)
        .get_project(&ident, &[])
        .await
        .unwrap();
    assert_eq!(project.identifier, "demo");
}

#[tokio::test]
async fn get_project_happy_path_by_id() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/projects/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(project_json(1, "demo")))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let ident = ProjectIdent::Id(ProjectId(1));
    let project = client
        .as_user(&cred)
        .get_project(&ident, &[])
        .await
        .unwrap();
    assert_eq!(project.id, 1);
}

#[tokio::test]
async fn get_project_not_found() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/projects/missing.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let ident = ProjectIdent::Identifier("missing".parse().unwrap());
    let err = client
        .as_user(&cred)
        .get_project(&ident, &[])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound));
}

#[tokio::test]
async fn list_projects_forbidden() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/projects.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let err = client
        .as_user(&cred)
        .list_projects(&ProjectQuery::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Forbidden));
}

#[tokio::test]
async fn get_project_with_issue_custom_fields_sends_the_include_param_and_parses_definitions() {
    let (server, client) = support::mock_redmine().await;
    let fixture = include_str!("fixtures/project_with_issue_custom_fields.json");
    let body: serde_json::Value = serde_json::from_str(fixture).unwrap();
    Mock::given(method("GET"))
        .and(path("/projects/1.json"))
        .and(query_param("include", "issue_custom_fields"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let cred = Credential::ApiKey(SecretString::from("k"));
    let ident = ProjectIdent::Id(ProjectId(1));
    let project = client
        .as_user(&cred)
        .get_project(&ident, &[ProjectInclude::IssueCustomFields])
        .await
        .unwrap();
    let defs = project
        .issue_custom_fields
        .expect("issue_custom_fields should be Some");
    assert_eq!(defs.len(), 3);
    let severity = defs.iter().find(|d| d.name == "Severity").unwrap();
    assert_eq!(severity.default_value.as_deref(), Some("Low"));
    assert_eq!(
        severity.possible_values,
        Some(vec!["Low".to_string(), "High".to_string()])
    );
}

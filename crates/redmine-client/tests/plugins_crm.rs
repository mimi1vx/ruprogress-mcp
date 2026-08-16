//! `RedmineUP` CRM: `GET/POST /contacts.json`, `GET/PUT/DELETE
//! /contacts/{id}.json`, `POST /contacts/{id}/projects.json`, `DELETE
//! /contacts/{id}/projects/{pid}.json`. Synthetic fixtures — see
//! `tests/fixtures/README.md`'s plugin fixtures section.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::str::FromStr as _;

use redmine_client::ProjectIdentifier;
use redmine_client::model::plugins::crm::{Contact, ContactInclude, ContactQuery, ContactWrite};
use redmine_client::{ContactId, Credential, Error, ProjectId, ProjectIdent};
use secrecy::SecretString;
use wiremock::matchers::{body_json, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, ResponseTemplate};

fn cred() -> Credential {
    Credential::ApiKey(SecretString::from("k"))
}

#[tokio::test]
async fn list_contacts_sends_the_requested_filters_and_pagination() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/contacts.json"))
        .and(query_param("search", "ada"))
        .and(query_param("tags", "vip,eng"))
        .and(query_param("assigned_to_id", "3"))
        .and(query_param("limit", "50"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_string(support::fixture("contacts_page")))
        .mount(&server)
        .await;

    let q = ContactQuery {
        search: Some("ada".to_string()),
        tags: Some("vip,eng".to_string()),
        assigned_to_id: Some(3),
        ..ContactQuery::default()
    };
    let page = client
        .as_user(&cred())
        .list_contacts(&q, 50, 0)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.total_count, 2);
}

#[tokio::test]
async fn list_contacts_errors_loudly_when_the_pagination_envelope_is_missing() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/contacts.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "contacts": []
        })))
        .mount(&server)
        .await;

    let err = client
        .as_user(&cred())
        .list_contacts(&ContactQuery::default(), 50, 0)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode { .. }));
}

#[tokio::test]
async fn get_contact_with_no_include_sends_no_include_param() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/contacts/1.json"))
        .and(query_param_is_missing("include"))
        .respond_with(ResponseTemplate::new(200).set_body_string(support::fixture("contact_full")))
        .mount(&server)
        .await;

    let contact: Contact = client
        .as_user(&cred())
        .get_contact(ContactId(1), &[])
        .await
        .unwrap();
    assert_eq!(contact.first_name.as_deref(), Some("Ada"));
    assert_eq!(contact.address.unwrap().city.as_deref(), Some("London"));
}

#[tokio::test]
async fn get_contact_with_includes_joins_them_with_commas() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/contacts/1.json"))
        .and(query_param("include", "notes,deals"))
        .respond_with(ResponseTemplate::new(200).set_body_string(support::fixture("contact_full")))
        .mount(&server)
        .await;

    client
        .as_user(&cred())
        .get_contact(
            ContactId(1),
            &[ContactInclude::Notes, ContactInclude::Deals],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn get_contact_minimal_fixture_round_trips() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/contacts/2.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("contact_minimal")),
        )
        .mount(&server)
        .await;

    let contact = client
        .as_user(&cred())
        .get_contact(ContactId(2), &[])
        .await
        .unwrap();
    assert_eq!(contact.first_name.as_deref(), Some("Grace"));
    assert!(contact.address.is_none());
}

#[tokio::test]
async fn create_contact_sends_exactly_the_set_fields() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/contacts.json"))
        .and(body_json(serde_json::json!({
            "contact": {"first_name": "Ada", "project_id": "my-project"}
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("contact_minimal")),
        )
        .mount(&server)
        .await;

    let new = ContactWrite {
        first_name: Some("Ada".to_string()),
        project_id: Some(ProjectIdent::Identifier(
            ProjectIdentifier::from_str("my-project").unwrap(),
        )),
        ..ContactWrite::default()
    };
    let contact = client.as_user(&cred()).create_contact(&new).await.unwrap();
    assert_eq!(contact.id, 2);
}

#[tokio::test]
async fn update_contact_sends_nested_address_attributes_then_fetches_the_fresh_resource() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("PUT"))
        .and(path("/contacts/1.json"))
        .and(body_json(serde_json::json!({
            "contact": {
                "address_attributes": {"city": "Paris"}
            }
        })))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/contacts/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(support::fixture("contact_full")))
        .mount(&server)
        .await;

    let patch = ContactWrite {
        address_attributes: Some(redmine_client::model::plugins::crm::ContactAddressWrite {
            city: Some("Paris".to_string()),
            ..Default::default()
        }),
        ..ContactWrite::default()
    };
    let contact = client
        .as_user(&cred())
        .update_contact(ContactId(1), &patch)
        .await
        .unwrap();
    assert_eq!(contact.first_name.as_deref(), Some("Ada"));
}

#[tokio::test]
async fn update_contact_forbidden() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("PUT"))
        .and(path("/contacts/1.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let patch = ContactWrite {
        first_name: Some("x".to_string()),
        ..ContactWrite::default()
    };
    let err = client
        .as_user(&cred())
        .update_contact(ContactId(1), &patch)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Forbidden));
}

#[tokio::test]
async fn delete_contact_succeeds_on_200() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("DELETE"))
        .and(path("/contacts/1.json"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    client
        .as_user(&cred())
        .delete_contact(ContactId(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn assign_contact_to_project_sends_the_expected_body() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/contacts/1/projects.json"))
        .and(body_json(serde_json::json!({
            "project": {"project_id": "5"}
        })))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let project = ProjectIdent::Id(ProjectId(5));
    client
        .as_user(&cred())
        .assign_contact_to_project(ContactId(1), &project)
        .await
        .unwrap();
}

#[tokio::test]
async fn remove_contact_from_project_hits_the_expected_path() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("DELETE"))
        .and(path("/contacts/1/projects/5.json"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let project = ProjectIdent::Id(ProjectId(5));
    client
        .as_user(&cred())
        .remove_contact_from_project(ContactId(1), &project)
        .await
        .unwrap();
}

/// A hostile project identifier is rejected at construction time
/// (`ProjectIdentifier::from_str`, covered exhaustively in `ids.rs`'s own
/// tests) — before it could ever reach the `remove_from_project` path built
/// here, so no request is ever sent.
#[tokio::test]
async fn a_traversal_project_identifier_cannot_reach_the_remove_from_project_path() {
    assert!(ProjectIdentifier::from_str("../../etc/passwd").is_err());
    let ident = ProjectIdent::Id(ProjectId(5));
    assert_eq!(
        format!("contacts/1/projects/{ident}.json"),
        "contacts/1/projects/5.json"
    );
}

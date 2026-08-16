//! `RedmineUP` Products: `GET /products.json`, `GET
//! /projects/{pid}/products.json`, `GET/POST/PUT /products/{id}.json`.
//! Synthetic fixtures — see `tests/fixtures/README.md`'s plugin fixtures
//! section.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::str::FromStr as _;

use redmine_client::model::plugins::products::{ProductQuery, ProductWrite};
use redmine_client::{Credential, Error, ProductId, ProjectId, ProjectIdent, ProjectIdentifier};
use secrecy::SecretString;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

fn cred() -> Credential {
    Credential::ApiKey(SecretString::from("k"))
}

#[tokio::test]
async fn list_products_with_no_project_hits_the_global_endpoint() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/products.json"))
        .and(query_param("limit", "50"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_string(support::fixture("products_page")))
        .mount(&server)
        .await;

    let page = client
        .as_user(&cred())
        .list_products(&ProductQuery::default(), 50, 0)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.total_count, 2);
    assert_eq!(page.items[0].name, "Widget");
}

#[tokio::test]
async fn list_products_with_a_project_hits_the_project_scoped_endpoint() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/projects/my-project/products.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(support::fixture("products_page")))
        .mount(&server)
        .await;

    let q = ProductQuery {
        project_id: Some(ProjectIdent::Identifier(
            ProjectIdentifier::from_str("my-project").unwrap(),
        )),
    };
    let page = client
        .as_user(&cred())
        .list_products(&q, 50, 0)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
}

#[tokio::test]
async fn list_products_errors_loudly_when_the_pagination_envelope_is_missing() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/products.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "products": []
        })))
        .mount(&server)
        .await;

    let err = client
        .as_user(&cred())
        .list_products(&ProductQuery::default(), 50, 0)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode { .. }));
}

#[tokio::test]
async fn get_product_full_fixture_round_trips() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/products/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(support::fixture("product_full")))
        .mount(&server)
        .await;

    let product = client
        .as_user(&cred())
        .get_product(ProductId(1))
        .await
        .unwrap();
    assert_eq!(product.name, "Widget");
    assert_eq!(product.custom_fields.unwrap().len(), 1);
}

#[tokio::test]
async fn get_product_minimal_fixture_round_trips() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("GET"))
        .and(path("/products/2.json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("product_minimal")),
        )
        .mount(&server)
        .await;

    let product = client
        .as_user(&cred())
        .get_product(ProductId(2))
        .await
        .unwrap();
    assert_eq!(product.name, "Gadget");
    assert_eq!(product.description, None);
}

#[tokio::test]
async fn create_product_sends_exactly_the_set_fields() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/products.json"))
        .and(body_json(serde_json::json!({
            "product": {"name": "Widget", "price": 9.99}
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(support::fixture("product_minimal")),
        )
        .mount(&server)
        .await;

    let new = ProductWrite {
        name: Some("Widget".to_string()),
        price: Some(9.99),
        ..ProductWrite::default()
    };
    let product = client.as_user(&cred()).create_product(&new).await.unwrap();
    assert_eq!(product.id, 2);
}

#[tokio::test]
async fn update_product_sends_exactly_the_set_fields_then_fetches_the_fresh_resource() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("PUT"))
        .and(path("/products/1.json"))
        .and(body_json(serde_json::json!({
            "product": {"status_id": 2}
        })))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/products/1.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(support::fixture("product_full")))
        .mount(&server)
        .await;

    let patch = ProductWrite {
        status_id: Some(2),
        ..ProductWrite::default()
    };
    let product = client
        .as_user(&cred())
        .update_product(ProductId(1), &patch)
        .await
        .unwrap();
    assert_eq!(product.name, "Widget");
}

#[tokio::test]
async fn update_product_forbidden() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("PUT"))
        .and(path("/products/1.json"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let patch = ProductWrite {
        name: Some("x".to_string()),
        ..ProductWrite::default()
    };
    let err = client
        .as_user(&cred())
        .update_product(ProductId(1), &patch)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Forbidden));
}

/// A hostile project identifier is rejected at construction time
/// (`ProjectIdentifier::from_str`, covered exhaustively in `ids.rs`'s own
/// tests) — before it could ever reach the project-scoped product list
/// path built here, so no request is ever sent.
#[tokio::test]
async fn a_traversal_project_identifier_cannot_reach_the_product_list_path() {
    assert!(ProjectIdentifier::from_str("../../etc/passwd").is_err());
    assert!(ProjectIdentifier::from_str("%2e%2e").is_err());
    // Sanity: a real numeric id still resolves to a normal path segment.
    let ident = ProjectIdent::Id(ProjectId(5));
    assert_eq!(
        format!("projects/{ident}/products.json"),
        "projects/5/products.json"
    );
}

//! `Scoped::introspect_token`/`revoke_token`: RFC 7662/7009 wire shape,
//! Basic-auth client credentials, and error mapping.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::items_after_statements
)]

mod support;

use redmine_client::{Credential, Error};
use secrecy::SecretString;
use wiremock::matchers::{basic_auth, body_string_contains, method, path};
use wiremock::{Mock, ResponseTemplate};

fn client_credential() -> Credential {
    Credential::Basic {
        user: "introspect-client".to_string(),
        pass: SecretString::from("introspect-secret"),
    }
}

#[tokio::test]
async fn introspect_token_sends_basic_auth_and_the_exact_form_body() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/oauth/introspect"))
        .and(basic_auth("introspect-client", "introspect-secret"))
        .and(body_string_contains("token=the-token"))
        .and(body_string_contains("token_type_hint=access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "active": true,
            "scope": "view_issues edit_issues",
            "username": "alice",
        })))
        .mount(&server)
        .await;

    let cred = client_credential();
    let result = client
        .as_user(&cred)
        .introspect_token(&SecretString::from("the-token"))
        .await
        .expect("introspection should succeed");

    assert!(result.active);
    assert_eq!(result.username.as_deref(), Some("alice"));
    assert_eq!(result.scopes(), vec!["view_issues", "edit_issues"]);
}

#[tokio::test]
async fn introspect_token_parses_an_inactive_response() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/oauth/introspect"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "active": false })),
        )
        .mount(&server)
        .await;

    let cred = client_credential();
    let result = client
        .as_user(&cred)
        .introspect_token(&SecretString::from("unknown-token"))
        .await
        .expect("an inactive token is a 200, not an error");
    assert!(!result.active);
}

#[tokio::test]
async fn introspect_token_maps_401_to_unauthorized() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/oauth/introspect"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let cred = client_credential();
    let err = client
        .as_user(&cred)
        .introspect_token(&SecretString::from("the-token"))
        .await
        .expect_err("401 should be an error");
    assert!(matches!(err, Error::Unauthorized));
}

#[tokio::test]
async fn introspect_token_maps_404_to_not_found() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/oauth/introspect"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let cred = client_credential();
    let err = client
        .as_user(&cred)
        .introspect_token(&SecretString::from("the-token"))
        .await
        .expect_err("404 should be an error");
    assert!(matches!(err, Error::NotFound));
}

#[tokio::test]
async fn introspect_token_error_never_contains_the_token() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/oauth/introspect"))
        .respond_with(ResponseTemplate::new(500).set_body_string("not json"))
        .mount(&server)
        .await;

    let cred = client_credential();
    const TOKEN: &str = "super-secret-access-token-xyz";
    let err = client
        .as_user(&cred)
        .introspect_token(&SecretString::from(TOKEN))
        .await
        .expect_err("500 should be an error");
    let display = format!("{err}");
    let debug = format!("{err:?}");
    assert!(
        !display.contains(TOKEN),
        "Display leaked the token: {display}"
    );
    assert!(!debug.contains(TOKEN), "Debug leaked the token: {debug}");
}

#[tokio::test]
async fn revoke_token_sends_basic_auth_and_the_form_body() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .and(basic_auth("introspect-client", "introspect-secret"))
        .and(body_string_contains("token=the-token"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let cred = client_credential();
    client
        .as_user(&cred)
        .revoke_token(&SecretString::from("the-token"), Some("access_token"))
        .await
        .expect("revocation should succeed");
}

#[tokio::test]
async fn revoke_token_of_an_unknown_token_is_still_success() {
    // RFC 7009: revoking a token the server does not recognise is a 200, not
    // an error.
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let cred = client_credential();
    client
        .as_user(&cred)
        .revoke_token(&SecretString::from("never-issued"), None)
        .await
        .expect("revoking an unknown token must not be an error");
}

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

// --- exchange_authorization_code (F5) ---------------------------------------

#[tokio::test]
async fn exchange_sends_basic_auth_and_the_exact_form_body_with_no_client_id() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(basic_auth("upstream-client", "upstream-secret"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=the-code"))
        .and(body_string_contains(
            "redirect_uri=https%3A%2F%2Fmcp.example.com%2Fauth%2Fcallback",
        ))
        .and(body_string_contains("code_verifier=the-verifier"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "upstream-access-token",
            "refresh_token": "upstream-refresh-token",
            "expires_in": 7200,
            "scope": "view_issues",
        })))
        .mount(&server)
        .await;

    let cred = Credential::Basic {
        user: "upstream-client".to_string(),
        pass: SecretString::from("upstream-secret"),
    };
    let token = client
        .as_user(&cred)
        .exchange_authorization_code(
            "the-code",
            "https://mcp.example.com/auth/callback",
            "the-verifier",
        )
        .await
        .expect("exchange should succeed");

    use secrecy::ExposeSecret;
    assert_eq!(token.access_token.expose_secret(), "upstream-access-token");
    assert_eq!(
        token
            .refresh_token
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some("upstream-refresh-token")
    );
    assert_eq!(token.expires_in, Some(7200));

    // Never sent as a form field: the Basic-auth header already
    // authenticates the client per RFC 6749 §3.2.1.
    let requests = server.received_requests().await.expect("requests recorded");
    let request = requests.first().expect("one request recorded");
    let body = String::from_utf8(request.body.clone()).expect("utf8 body");
    assert!(!body.contains("client_id="));
}

#[tokio::test]
async fn exchange_maps_a_400_invalid_grant_body_to_error_oauth() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant",
            "error_description": "the code has expired",
        })))
        .mount(&server)
        .await;

    let cred = client_credential();
    let err = client
        .as_user(&cred)
        .exchange_authorization_code("stale-code", "https://mcp.example.com/auth/callback", "v")
        .await
        .expect_err("400 should be an error");
    match err {
        Error::OAuth {
            status,
            error,
            description,
        } => {
            assert_eq!(status, 400);
            assert_eq!(error, "invalid_grant");
            assert_eq!(description.as_deref(), Some("the code has expired"));
        }
        other => panic!("expected Error::OAuth, got {other:?}"),
    }
}

#[tokio::test]
async fn exchange_error_never_contains_the_code_or_verifier() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant",
        })))
        .mount(&server)
        .await;

    let cred = client_credential();
    const CODE: &str = "super-secret-code-xyz";
    const VERIFIER: &str = "super-secret-verifier-xyz";
    let err = client
        .as_user(&cred)
        .exchange_authorization_code(CODE, "https://mcp.example.com/auth/callback", VERIFIER)
        .await
        .expect_err("400 should be an error");
    let display = format!("{err}");
    let debug = format!("{err:?}");
    for secret in [CODE, VERIFIER] {
        assert!(!display.contains(secret), "Display leaked: {display}");
        assert!(!debug.contains(secret), "Debug leaked: {debug}");
    }
}

// --- refresh_access_token (R1) ------------------------------------------

#[tokio::test]
async fn refresh_sends_basic_auth_and_the_exact_form_body_with_no_client_id() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(basic_auth("upstream-client", "upstream-secret"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=the-refresh-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "new-upstream-access-token",
            "refresh_token": "new-upstream-refresh-token",
            "expires_in": 7200,
            "scope": "view_issues",
        })))
        .mount(&server)
        .await;

    let cred = Credential::Basic {
        user: "upstream-client".to_string(),
        pass: SecretString::from("upstream-secret"),
    };
    let token = client
        .as_user(&cred)
        .refresh_access_token(&SecretString::from("the-refresh-token"))
        .await
        .expect("refresh should succeed");

    use secrecy::ExposeSecret;
    assert_eq!(
        token.access_token.expose_secret(),
        "new-upstream-access-token"
    );
    assert_eq!(
        token
            .refresh_token
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some("new-upstream-refresh-token")
    );

    // Never sent as a form field: the Basic-auth header already
    // authenticates the client per RFC 6749 §3.2.1.
    let requests = server.received_requests().await.expect("requests recorded");
    let request = requests.first().expect("one request recorded");
    let body = String::from_utf8(request.body.clone()).expect("utf8 body");
    assert!(!body.contains("client_id="));
}

#[tokio::test]
async fn refresh_maps_a_400_invalid_grant_body_to_error_oauth() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant",
            "error_description": "the refresh token has been revoked",
        })))
        .mount(&server)
        .await;

    let cred = client_credential();
    let err = client
        .as_user(&cred)
        .refresh_access_token(&SecretString::from("stale-refresh-token"))
        .await
        .expect_err("400 should be an error");
    match err {
        Error::OAuth {
            status,
            error,
            description,
        } => {
            assert_eq!(status, 400);
            assert_eq!(error, "invalid_grant");
            assert_eq!(
                description.as_deref(),
                Some("the refresh token has been revoked")
            );
        }
        other => panic!("expected Error::OAuth, got {other:?}"),
    }
}

#[tokio::test]
async fn refresh_error_never_contains_the_refresh_token() {
    let (server, client) = support::mock_redmine().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant",
        })))
        .mount(&server)
        .await;

    let cred = client_credential();
    const REFRESH_TOKEN: &str = "super-secret-refresh-token-xyz";
    let err = client
        .as_user(&cred)
        .refresh_access_token(&SecretString::from(REFRESH_TOKEN))
        .await
        .expect_err("400 should be an error");
    let display = format!("{err}");
    let debug = format!("{err:?}");
    assert!(
        !display.contains(REFRESH_TOKEN),
        "Display leaked: {display}"
    );
    assert!(!debug.contains(REFRESH_TOKEN), "Debug leaked: {debug}");
}

//! `POST /register` (RFC 7591 §3, C6): open Dynamic Client Registration —
//! no initial access token, every registered client public.
//!
//! SECURITY: this route is unauthenticated and allocates a registry slot on
//! every valid call. Bounded today by [`crate::oauth::proxy::store::ClientRegistry`]'s
//! cap and LRU-idle eviction; a natural first customer of a future rate
//! limiter.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use http::{HeaderValue, StatusCode, header};
use serde_json::json;

use super::redirect::RedirectPolicy;
use super::store::ClientRegistry;

/// Every response (success or error) carries `Cache-Control: no-store` —
/// set directly rather than via a router layer, so it holds regardless of
/// how the handler is invoked.
fn no_store(status: StatusCode, body: serde_json::Value) -> Response {
    let mut response = (status, axum::Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Bytes cap for the registration body: far above any real RFC 7591
/// request, far below anything worth reading in full before rejecting.
const MAX_REGISTER_BODY_BYTES: usize = 8 * 1024;

/// The RFC 7591 §2 fields this server reads. Anything else in the request
/// body is silently dropped by `serde`'s default (non-`deny_unknown_fields`)
/// behaviour — never echoed, never stored.
#[derive(Debug, serde::Deserialize)]
struct RegisterRequest {
    redirect_uris: Vec<String>,
    #[serde(default)]
    client_name: Option<String>,
    #[serde(default)]
    grant_types: Option<Vec<String>>,
    #[serde(default)]
    response_types: Option<Vec<String>>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
    // Named so a value of the wrong JSON type is rejected as
    // `invalid_client_metadata` rather than silently accepted as an unknown
    // field, but never echoed or stored: this server does not restrict a
    // client's advertised OAuth scope or application type — Redmine's own
    // introspection is the authority on what a *token* may do (P9).
    #[serde(default)]
    #[allow(
        dead_code,
        reason = "read for type validation only, per C6 — never echoed or stored"
    )]
    scope: Option<String>,
    #[serde(default)]
    #[allow(
        dead_code,
        reason = "read for type validation only, per C6 — never echoed or stored"
    )]
    application_type: Option<String>,
}

#[derive(Clone)]
struct RegisterState {
    registry: Arc<ClientRegistry>,
    redirects: Arc<RedirectPolicy>,
}

/// Mounts `POST /register`. Only ever called for `AuthMode::OAuthProxy` —
/// see `transport::http::router`.
pub(crate) fn register_route(
    registry: Arc<ClientRegistry>,
    redirects: Arc<RedirectPolicy>,
) -> Router {
    Router::new()
        .route("/register", post(register))
        .with_state(RegisterState {
            registry,
            redirects,
        })
}

fn content_type_is_json(request: &Request) -> bool {
    request
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
}

fn client_metadata_error(description: &str) -> Response {
    no_store(
        StatusCode::BAD_REQUEST,
        json!({
            "error": "invalid_client_metadata",
            "error_description": description,
        }),
    )
}

fn redirect_uri_error(description: &str) -> Response {
    no_store(
        StatusCode::BAD_REQUEST,
        json!({
            "error": "invalid_redirect_uri",
            "error_description": description,
        }),
    )
}

const SUPPORTED_GRANT_TYPES: &[&str] = &["authorization_code", "refresh_token"];
const SUPPORTED_RESPONSE_TYPES: &[&str] = &["code"];

async fn register(State(state): State<RegisterState>, request: Request) -> Response {
    if !content_type_is_json(&request) {
        let mut response = (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported media type: expected application/json",
        )
            .into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return response;
    }

    let Ok(bytes) = axum::body::to_bytes(request.into_body(), MAX_REGISTER_BODY_BYTES).await else {
        let mut response = (StatusCode::PAYLOAD_TOO_LARGE, "payload too large").into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return response;
    };

    let payload: RegisterRequest = match serde_json::from_slice(&bytes) {
        Ok(payload) => payload,
        Err(error) => {
            return client_metadata_error(&format!("the request body is not valid: {error}"));
        }
    };

    if let Some(grant_types) = &payload.grant_types
        && let Some(unsupported) = grant_types
            .iter()
            .find(|g| !SUPPORTED_GRANT_TYPES.contains(&g.as_str()))
    {
        return client_metadata_error(&format!(
            "grant_types entry {unsupported:?} is not supported"
        ));
    }
    if let Some(response_types) = &payload.response_types
        && let Some(unsupported) = response_types
            .iter()
            .find(|r| !SUPPORTED_RESPONSE_TYPES.contains(&r.as_str()))
    {
        return client_metadata_error(&format!(
            "response_types entry {unsupported:?} is not supported"
        ));
    }
    if let Some(method) = &payload.token_endpoint_auth_method
        && method != "none"
    {
        return client_metadata_error(&format!(
            "token_endpoint_auth_method {method:?} is not supported: every client registered \
             here is public (\"none\")"
        ));
    }
    if payload.redirect_uris.is_empty() {
        return client_metadata_error("redirect_uris must contain at least one entry");
    }
    if let Some(rejected) = payload
        .redirect_uris
        .iter()
        .find(|uri| !state.redirects.permits(uri))
    {
        return redirect_uri_error(&format!("redirect_uri {rejected:?} is not allowed"));
    }

    let Some(registration) = state
        .registry
        .register(payload.redirect_uris.clone(), payload.client_name.clone())
    else {
        let mut response = no_store(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({
                "error": "temporarily_unavailable",
                "error_description": "the client registry is full; try again shortly",
            }),
        );
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
        return response;
    };

    let mut fields = serde_json::Map::new();
    fields.insert("client_id".to_string(), json!(registration.client_id));
    fields.insert(
        "client_id_issued_at".to_string(),
        json!(chrono::Utc::now().timestamp()),
    );
    fields.insert(
        "redirect_uris".to_string(),
        json!(registration.redirect_uris),
    );
    fields.insert("token_endpoint_auth_method".to_string(), json!("none"));
    fields.insert("grant_types".to_string(), json!(SUPPORTED_GRANT_TYPES));
    fields.insert(
        "response_types".to_string(),
        json!(SUPPORTED_RESPONSE_TYPES),
    );
    if let Some(name) = &registration.client_name {
        fields.insert("client_name".to_string(), json!(name));
    }
    no_store(StatusCode::CREATED, serde_json::Value::Object(fields))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use axum::body::Body;

    use super::*;

    fn state() -> RegisterState {
        RegisterState {
            registry: Arc::new(ClientRegistry::new()),
            redirects: Arc::new(RedirectPolicy::Loopback),
        }
    }

    fn json_request(body: &str) -> Request {
        Request::builder()
            .method("POST")
            .uri("/register")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request should build")
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        serde_json::from_slice(&bytes).expect("body should be json")
    }

    #[tokio::test]
    async fn happy_path_returns_a_client_id_and_no_client_secret() {
        let response = register(
            State(state()),
            json_request(r#"{"redirect_uris": ["http://localhost:4000/cb"]}"#),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response).await;
        assert!(body["client_id"].as_str().is_some());
        assert!(body.get("client_secret").is_none());
        assert_eq!(body["token_endpoint_auth_method"], "none");
    }

    #[tokio::test]
    async fn a_non_loopback_uri_is_rejected_by_the_default_policy() {
        let response = register(
            State(state()),
            json_request(r#"{"redirect_uris": ["https://app.example.com/cb"]}"#),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["error"], "invalid_redirect_uri");
    }

    #[tokio::test]
    async fn a_non_loopback_uri_is_accepted_once_the_policy_allows_it() {
        let state = RegisterState {
            registry: Arc::new(ClientRegistry::new()),
            redirects: Arc::new(RedirectPolicy::Patterns(vec![
                super::super::redirect::RedirectPattern::parse("https://app.example.com/*")
                    .expect("valid pattern"),
            ])),
        };
        let response = register(
            State(state),
            json_request(r#"{"redirect_uris": ["https://app.example.com/cb"]}"#),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn client_secret_post_auth_method_is_invalid_client_metadata() {
        let response = register(
            State(state()),
            json_request(
                r#"{"redirect_uris": ["http://localhost/cb"], "token_endpoint_auth_method": "client_secret_post"}"#,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["error"], "invalid_client_metadata");
    }

    #[tokio::test]
    async fn unknown_fields_are_ignored_and_never_echoed() {
        let response = register(
            State(state()),
            json_request(
                r#"{"redirect_uris": ["http://localhost/cb"], "some_unknown_field": "attacker-controlled"}"#,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response).await;
        assert!(!body.to_string().contains("attacker-controlled"));
        assert!(body.get("some_unknown_field").is_none());
    }

    #[tokio::test]
    async fn missing_redirect_uris_is_invalid_client_metadata() {
        let response = register(State(state()), json_request(r"{}")).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["error"], "invalid_client_metadata");
    }

    #[tokio::test]
    async fn empty_redirect_uris_is_invalid_client_metadata() {
        let response = register(State(state()), json_request(r#"{"redirect_uris": []}"#)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["error"], "invalid_client_metadata");
    }

    #[tokio::test]
    async fn unsupported_grant_type_is_invalid_client_metadata() {
        let response = register(
            State(state()),
            json_request(
                r#"{"redirect_uris": ["http://localhost/cb"], "grant_types": ["implicit"]}"#,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["error"], "invalid_client_metadata");
    }

    #[tokio::test]
    async fn non_json_content_type_is_unsupported_media_type() {
        let request = Request::builder()
            .method("POST")
            .uri("/register")
            .header(
                http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(Body::from("redirect_uris=http://localhost/cb"))
            .expect("request should build");
        let response = register(State(state()), request).await;
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn oversized_body_is_payload_too_large() {
        let huge = format!(
            r#"{{"redirect_uris": ["http://localhost/cb"], "client_name": "{}"}}"#,
            "a".repeat(MAX_REGISTER_BODY_BYTES)
        );
        let response = register(State(state()), json_request(&huge)).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn malformed_json_is_invalid_client_metadata() {
        let response = register(State(state()), json_request("not json")).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["error"], "invalid_client_metadata");
    }

    #[tokio::test]
    async fn response_carries_no_store_cache_control() {
        let response = register(
            State(state()),
            json_request(r#"{"redirect_uris": ["http://localhost/cb"]}"#),
        )
        .await;
        assert_eq!(
            response.headers().get(http::header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
    }
}

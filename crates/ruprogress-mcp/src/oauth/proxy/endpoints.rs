//! `POST /register` (RFC 7591 §3, C6): open Dynamic Client Registration —
//! no initial access token, every registered client public. Also `GET
//! /authorize`, `GET /auth/callback`, and `POST /token` (F1–F9): the
//! downstream authorization-code + PKCE flow this server runs as an
//! authorization server, and the upstream one it runs as an OAuth client
//! against Redmine to fulfil it.
//!
//! SECURITY: every route in this module is unauthenticated and allocates
//! state on a valid call. Bounded by each store's own cap in
//! [`crate::oauth::proxy::store`]; a natural first customer of a future rate
//! limiter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Query, Request, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use http::{HeaderValue, StatusCode, header};
use redmine_client::{Credential, RedmineClient};
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::json;
use url::Url;

use super::pkce;
use super::redirect::RedirectPolicy;
use super::store::{
    ProxyState, RedeemOutcome, RefreshOutcome, Transaction, UpstreamTokenSet, expires_after,
};
use crate::auth::oauth::TokenVerifier;
use crate::config::OAuthConfig;
use crate::oauth::scopes;

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
    proxy: Arc<ProxyState>,
    redirects: Arc<RedirectPolicy>,
}

/// Mounts `POST /register`. Only ever called for `AuthMode::OAuthProxy` —
/// see `transport::http::router`.
pub(crate) fn register_route(proxy: Arc<ProxyState>, redirects: Arc<RedirectPolicy>) -> Router {
    Router::new()
        .route("/register", post(register))
        .with_state(RegisterState { proxy, redirects })
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

/// A private copy of `transport::http`'s check of the same name: both exist
/// to keep each module's route handlers free-standing, and the check itself
/// is a two-line `Content-Type` comparison not worth sharing across a module
/// boundary for.
fn content_type_is_form(request: &Request) -> bool {
    request
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/x-www-form-urlencoded")
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
        .proxy
        .register_client(payload.redirect_uris.clone(), payload.client_name.clone())
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

// --- shared flow state and helpers (F1–F9) ----------------------------------

/// Shared by `/authorize`, `/auth/callback`, `/token`, and `/revoke`.
/// `proxy` is also handed to `auth::proxy`'s middleware by
/// `transport::http::router`, so a token minted here is resolvable there
/// without a second store.
#[derive(Clone)]
struct FlowState {
    proxy: Arc<ProxyState>,
    redirects: Arc<RedirectPolicy>,
    oauth: Arc<OAuthConfig>,
    redmine_base: Url,
    upstream_client_id: String,
    upstream_client_secret: SecretString,
    redmine: RedmineClient,
    /// The same introspection verifier `auth::proxy`'s middleware uses, so
    /// killing an upstream token here (rotation reuse, upstream refresh
    /// failure, `/revoke`) also purges it from that cache (R6).
    verifier: Arc<TokenVerifier>,
}

/// Mounts `GET /authorize`, `GET /auth/callback`, `POST /token`, and `POST
/// /revoke`. Only ever called for `AuthMode::OAuthProxy` — see
/// `transport::http::router`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn flow_routes(
    proxy: Arc<ProxyState>,
    redirects: Arc<RedirectPolicy>,
    oauth: Arc<OAuthConfig>,
    redmine_base: Url,
    upstream_client_id: String,
    upstream_client_secret: SecretString,
    redmine: RedmineClient,
    verifier: Arc<TokenVerifier>,
) -> Router {
    Router::new()
        .route("/authorize", get(authorize))
        .route("/auth/callback", get(auth_callback))
        .route("/token", post(token))
        .route("/revoke", post(revoke))
        .with_state(FlowState {
            proxy,
            redirects,
            oauth,
            redmine_base,
            upstream_client_id,
            upstream_client_secret,
            redmine,
            verifier,
        })
}

fn trim_trailing_slash(raw: &str) -> &str {
    raw.strip_suffix('/').unwrap_or(raw)
}

/// Phase A failures (F1): plain text, no `Location` header, ever — a
/// redirect at this point would be the open redirect.
fn plain_400(message: &'static str) -> Response {
    (StatusCode::BAD_REQUEST, message).into_response()
}

/// Phase B and callback failures (F1, F4, P11): a `302` carrying `error`,
/// `error_description`, the client's `state` (if any), and RFC 9207 `iss` —
/// only ever built from a `redirect_uri` that has already passed Phase A.
fn redirect_error(
    redirect_uri: &str,
    client_state: Option<&str>,
    base_url: &Url,
    error: &str,
    description: &str,
) -> Response {
    let Ok(mut url) = Url::parse(redirect_uri) else {
        // Unreachable in practice (F1 already validated this exact string
        // as a URL), kept as a fail-closed plain 400 rather than a panic.
        return plain_400("redirect_uri is not a valid URL");
    };
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("error", error);
        pairs.append_pair("error_description", description);
        if let Some(state) = client_state {
            pairs.append_pair("state", state);
        }
        pairs.append_pair("iss", trim_trailing_slash(base_url.as_str()));
    }
    Redirect::to(url.as_str()).into_response()
}

// --- GET /authorize (F1, F13) -----------------------------------------------

#[derive(Debug, Deserialize)]
struct AuthorizeQuery {
    #[serde(default)]
    response_type: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code_challenge: Option<String>,
    #[serde(default)]
    code_challenge_method: Option<String>,
}

/// Phase A (F1): resolves `client_id` and validates `redirect_uri` against
/// both the client's registration and the current redirect policy. No
/// redirect is possible yet, so a failure is an `Err` message the caller
/// renders as a plain `400` with no `Location` header. `Err` is a
/// `&'static str`, not a built [`Response`]: the latter would trip
/// `clippy::result_large_err` for no benefit, since the caller builds the
/// response at its one call site either way — same reasoning as
/// `transport::http::validate_files_host`. Split out of [`authorize`] to
/// keep that function under clippy's line-count pedantic threshold.
fn authorize_phase_a(
    state: &FlowState,
    query: &AuthorizeQuery,
) -> Result<(String, String), &'static str> {
    let client_id = query
        .client_id
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or("client_id is required")?;
    let registration = state
        .proxy
        .registry
        .get(&client_id)
        .ok_or("client_id is not a registered client")?;
    // Any caller that can name a valid client_id may keep it alive, which
    // is the safe direction — this runs before redirect_uri is even
    // checked.
    state.proxy.registry.touch(&client_id);
    let redirect_uri = query
        .redirect_uri
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or("redirect_uri is required")?;
    if !registration
        .redirect_uris
        .iter()
        .any(|uri| uri == &redirect_uri)
    {
        return Err("redirect_uri is not registered for this client");
    }
    if !state.redirects.permits(&redirect_uri) {
        return Err("redirect_uri is no longer permitted by this deployment's redirect-URI policy");
    }
    Ok((client_id, redirect_uri))
}

async fn authorize(
    State(state): State<FlowState>,
    Query(query): Query<AuthorizeQuery>,
) -> Response {
    let (client_id, redirect_uri) = match authorize_phase_a(&state, &query) {
        Ok(pair) => pair,
        Err(message) => return plain_400(message),
    };
    authorize_phase_b(&state, &query, client_id, &redirect_uri)
}

/// Phase B (F1, F13): `response_type`, PKCE, and scope, then minting the
/// transaction and redirecting to Redmine's own authorize endpoint. A
/// redirect is safe from here on, since `redirect_uri` already passed
/// Phase A. Split out of [`authorize`] to keep that function under
/// clippy's line-count pedantic threshold.
fn authorize_phase_b(
    state: &FlowState,
    query: &AuthorizeQuery,
    client_id: String,
    redirect_uri: &str,
) -> Response {
    if query.response_type.as_deref() != Some("code") {
        return redirect_error(
            redirect_uri,
            query.state.as_deref(),
            &state.oauth.base_url,
            "unsupported_response_type",
            "response_type must be \"code\"",
        );
    }
    let Some(code_challenge) = query.code_challenge.clone().filter(|s| !s.is_empty()) else {
        return redirect_error(
            redirect_uri,
            query.state.as_deref(),
            &state.oauth.base_url,
            "invalid_request",
            "code_challenge is required",
        );
    };
    if query.code_challenge_method.as_deref() != Some("S256") {
        return redirect_error(
            redirect_uri,
            query.state.as_deref(),
            &state.oauth.base_url,
            "invalid_request",
            "code_challenge_method must be \"S256\"",
        );
    }
    let requested_scopes: Vec<&'static str> =
        match query.scope.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(raw) => match scopes::narrow(&state.oauth.scopes, raw) {
                Ok(narrowed) => narrowed,
                Err(_) => {
                    return redirect_error(
                        redirect_uri,
                        query.state.as_deref(),
                        &state.oauth.base_url,
                        "invalid_scope",
                        "one or more requested scopes are not advertised by this deployment",
                    );
                }
            },
            None => state.oauth.scopes.clone(),
        };

    let Some(upstream_verifier) = pkce::generate_verifier() else {
        return redirect_error(
            redirect_uri,
            query.state.as_deref(),
            &state.oauth.base_url,
            "temporarily_unavailable",
            "the authorization service is temporarily unable to start a new flow",
        );
    };
    let upstream_challenge = pkce::challenge_for(&upstream_verifier);

    let transaction = Transaction {
        client_id,
        redirect_uri: redirect_uri.to_string(),
        code_challenge,
        scopes: requested_scopes.iter().map(|s| (*s).to_string()).collect(),
        client_state: query.state.clone(),
        upstream_code_verifier: upstream_verifier,
    };
    let Some(transaction_id) = state.proxy.transactions.create(transaction) else {
        return redirect_error(
            redirect_uri,
            query.state.as_deref(),
            &state.oauth.base_url,
            "temporarily_unavailable",
            "the authorization service is at capacity; try again shortly",
        );
    };

    let Ok(mut upstream_authorize) = Url::parse(&format!(
        "{}/oauth/authorize",
        trim_trailing_slash(state.redmine_base.as_str())
    )) else {
        return redirect_error(
            redirect_uri,
            query.state.as_deref(),
            &state.oauth.base_url,
            "server_error",
            "this server's Redmine URL is misconfigured",
        );
    };
    upstream_authorize
        .query_pairs_mut()
        .append_pair("client_id", &state.upstream_client_id)
        .append_pair(
            "redirect_uri",
            &format!(
                "{}/auth/callback",
                trim_trailing_slash(state.oauth.base_url.as_str())
            ),
        )
        .append_pair("response_type", "code")
        .append_pair("scope", &requested_scopes.join(" "))
        .append_pair("state", &transaction_id)
        .append_pair("code_challenge", &upstream_challenge)
        .append_pair("code_challenge_method", "S256");

    Redirect::to(upstream_authorize.as_str()).into_response()
}

// --- GET /auth/callback (F2–F4, F6) -----------------------------------------

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

async fn auth_callback(
    State(state): State<FlowState>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(raw_state) = query.state.filter(|s| !s.is_empty()) else {
        return plain_400("state is required");
    };
    // F4: the transaction lookup itself has no redirect target to fail
    // toward — an unknown, expired, or already-used state cannot redirect
    // anywhere, by construction.
    let Some(transaction) = state.proxy.transactions.take(&raw_state) else {
        return plain_400("state is unknown, expired, or already used");
    };

    if let Some(error) = query.error.filter(|s| !s.is_empty()) {
        tracing::warn!(error, "upstream authorization denied or failed");
        return redirect_error(
            &transaction.redirect_uri,
            transaction.client_state.as_deref(),
            &state.oauth.base_url,
            &error,
            query
                .error_description
                .as_deref()
                .unwrap_or("the upstream authorization server returned an error"),
        );
    }

    let Some(code) = query.code.filter(|s| !s.is_empty()) else {
        return redirect_error(
            &transaction.redirect_uri,
            transaction.client_state.as_deref(),
            &state.oauth.base_url,
            "invalid_request",
            "the upstream authorization server did not return a code",
        );
    };

    let credential = Credential::Basic {
        user: state.upstream_client_id.clone(),
        pass: state.upstream_client_secret.clone(),
    };
    let callback_url = format!(
        "{}/auth/callback",
        trim_trailing_slash(state.oauth.base_url.as_str())
    );
    let exchange = state
        .redmine
        .as_user(&credential)
        .exchange_authorization_code(&code, &callback_url, &transaction.upstream_code_verifier)
        .await;

    let upstream_token = match exchange {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(%error, "upstream authorization-code exchange failed");
            return redirect_error(
                &transaction.redirect_uri,
                transaction.client_state.as_deref(),
                &state.oauth.base_url,
                "server_error",
                "failed to exchange the authorization code with the upstream authorization \
                 server",
            );
        }
    };

    let granted_scopes = upstream_token.scope.as_deref().map_or_else(
        || transaction.scopes.clone(),
        |raw| {
            raw.split_ascii_whitespace()
                .map(ToString::to_string)
                .collect()
        },
    );
    let expires_at = expires_after(Duration::from_secs(
        upstream_token.expires_in.unwrap_or(3600),
    ));
    let upstream_set = UpstreamTokenSet {
        access: upstream_token.access_token,
        refresh: upstream_token.refresh_token,
        granted_scopes,
        expires_at,
    };

    let Some(code_value) = state.proxy.codes.mint(
        transaction.client_id.clone(),
        transaction.redirect_uri.clone(),
        transaction.code_challenge.clone(),
        upstream_set,
    ) else {
        return redirect_error(
            &transaction.redirect_uri,
            transaction.client_state.as_deref(),
            &state.oauth.base_url,
            "temporarily_unavailable",
            "the authorization service is at capacity; try again shortly",
        );
    };

    let Ok(mut redirect_url) = Url::parse(&transaction.redirect_uri) else {
        return plain_400("stored redirect_uri is not a valid URL");
    };
    {
        let mut pairs = redirect_url.query_pairs_mut();
        pairs.append_pair("code", &code_value);
        if let Some(client_state) = &transaction.client_state {
            pairs.append_pair("state", client_state);
        }
        pairs.append_pair("iss", trim_trailing_slash(state.oauth.base_url.as_str()));
    }
    Redirect::to(redirect_url.as_str()).into_response()
}

// --- POST /token (F7–F9) -----------------------------------------------------

/// Bytes cap for the token request body: far above any real RFC 6749
/// `application/x-www-form-urlencoded` request, far below anything worth
/// reading in full before rejecting.
const MAX_TOKEN_BODY_BYTES: usize = 8 * 1024;

fn token_error(status: StatusCode, error: &str) -> Response {
    no_store(status, json!({ "error": error }))
}

/// Logged once per process, the first time an upstream token response omits
/// `refresh_token` (R4): the operator-facing signal that Doorkeeper's
/// `use_refresh_token` setting is off, rather than a silent per-request
/// downgrade to shorter, browser-driven sessions.
static NO_UPSTREAM_REFRESH_TOKEN_WARNED: std::sync::Once = std::sync::Once::new();

fn warn_no_upstream_refresh_token_once() {
    NO_UPSTREAM_REFRESH_TOKEN_WARNED.call_once(|| {
        tracing::warn!(
            "the upstream OAuth application never returns a refresh_token; enable Doorkeeper's \
             use_refresh_token setting, or clients must re-authorize in a browser every time \
             their access token expires"
        );
    });
}

/// What a successful (re)mint hands back to a `/token` caller. `*_digest`
/// let the `authorization_code` grant record exactly what it minted (F8),
/// so a later replay of the same code can revoke precisely those entries.
struct MintedPair {
    access_token: String,
    access_digest: [u8; 32],
    refresh_token: Option<String>,
    refresh_digest: Option<[u8; 32]>,
    ttl: Duration,
}

/// Mints a fresh proxy access token, plus a fresh proxy refresh token when
/// `wants_refresh` is true (R1, R4). `None` means a store was full or
/// `SysRng` failed (C8); the caller turns that into a `server_error`. Shared
/// by the `authorization_code` and `refresh_token` grants so the two mint
/// exactly the same shape of pair.
fn mint_token_pair(
    proxy: &ProxyState,
    client_id: &str,
    upstream_id: &str,
    ttl: Duration,
    wants_refresh: bool,
) -> Option<MintedPair> {
    let (access_token, access_digest) =
        proxy
            .tokens
            .mint(client_id.to_string(), upstream_id.to_string(), ttl)?;
    let (refresh_token, refresh_digest) = if wants_refresh {
        let (refresh_token, refresh_digest) = proxy
            .refresh_tokens
            .mint(client_id.to_string(), upstream_id.to_string())?;
        (Some(refresh_token), Some(refresh_digest))
    } else {
        warn_no_upstream_refresh_token_once();
        (None, None)
    };
    Some(MintedPair {
        access_token,
        access_digest,
        refresh_token,
        refresh_digest,
        ttl,
    })
}

fn token_response(pair: &MintedPair, scope: &str) -> Response {
    let mut fields = serde_json::Map::new();
    fields.insert("access_token".to_string(), json!(pair.access_token));
    fields.insert("token_type".to_string(), json!("Bearer"));
    fields.insert("expires_in".to_string(), json!(pair.ttl.as_secs()));
    fields.insert("scope".to_string(), json!(scope));
    if let Some(refresh_token) = &pair.refresh_token {
        fields.insert("refresh_token".to_string(), json!(refresh_token));
    }
    no_store(StatusCode::OK, serde_json::Value::Object(fields))
}

async fn token(State(state): State<FlowState>, request: Request) -> Response {
    if !content_type_is_form(&request) {
        let mut response = (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported media type: expected application/x-www-form-urlencoded",
        )
            .into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return response;
    }

    let Ok(bytes) = axum::body::to_bytes(request.into_body(), MAX_TOKEN_BODY_BYTES).await else {
        let mut response = (StatusCode::PAYLOAD_TOO_LARGE, "payload too large").into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return response;
    };

    let fields: HashMap<String, String> = url::form_urlencoded::parse(&bytes)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    match fields.get("grant_type").map(String::as_str) {
        Some("authorization_code") => authorization_code_grant(&state, &fields),
        Some("refresh_token") => refresh_token_grant(&state, &fields).await,
        Some(_) => token_error(StatusCode::BAD_REQUEST, "unsupported_grant_type"),
        None => token_error(StatusCode::BAD_REQUEST, "invalid_request"),
    }
}

/// Sweeps every session past its deadline (finding 6), purging each one's
/// access token from the introspection cache immediately, and — for
/// whichever carried a refresh token — revoking it upstream in a
/// best-effort background task kept off the request path. A session with
/// no refresh token triggers no upstream call at all: Doorkeeper silently
/// no-ops revoking an already-expired access token (V1), so there is
/// nothing revocable left. Called once per `authorization_code` grant, the
/// only session-creating path (finding 6's sweep trigger); an idle server
/// otherwise retains expired sessions until the next authorization,
/// bounded by `MAX_ENTRIES` rather than unbounded.
fn sweep_expired_sessions(state: &FlowState) {
    let swept = state.proxy.sweep_expired_sessions();
    if swept.is_empty() {
        return;
    }
    let mut refreshable = Vec::new();
    for set in swept {
        state.verifier.purge(&set.access);
        if set.refresh.is_some() {
            refreshable.push(set);
        }
    }
    if refreshable.is_empty() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        for set in refreshable {
            revoke_upstream_and_purge(&state, &set).await;
        }
    });
}

/// `grant_type=authorization_code` (F7–F9, R1, R4, R6).
fn authorization_code_grant(state: &FlowState, fields: &HashMap<String, String>) -> Response {
    let (Some(code), Some(redirect_uri), Some(client_id), Some(code_verifier)) = (
        fields.get("code"),
        fields.get("redirect_uri"),
        fields.get("client_id"),
        fields.get("code_verifier"),
    ) else {
        return token_error(StatusCode::BAD_REQUEST, "invalid_request");
    };

    match state
        .proxy
        .codes
        .redeem(code, client_id, redirect_uri, code_verifier)
    {
        RedeemOutcome::Invalid | RedeemOutcome::Mismatch => {
            token_error(StatusCode::BAD_REQUEST, "invalid_grant")
        }
        RedeemOutcome::Replayed {
            minted_token_digest,
            minted_refresh_digest,
            upstream_id,
        } => {
            state.proxy.tokens.delete_by_digest(minted_token_digest);
            if let Some(refresh_digest) = minted_refresh_digest {
                state.proxy.refresh_tokens.delete_by_digest(refresh_digest);
            }
            if let Some(upstream) = state.proxy.take_session(&upstream_id) {
                state.verifier.purge(&upstream.access);
            }
            tracing::warn!(
                client_id,
                "authorization code replay detected; revoked the tokens it minted"
            );
            token_error(StatusCode::BAD_REQUEST, "invalid_grant")
        }
        RedeemOutcome::Ok(upstream) => {
            // `redeem`'s Ok variant already validated the client binding,
            // so this use is trusted.
            state.proxy.registry.touch(client_id);
            sweep_expired_sessions(state);
            let ttl = upstream
                .expires_at
                .saturating_duration_since(Instant::now());
            let wants_refresh = upstream.refresh.is_some();
            let granted_scope = upstream.granted_scopes.join(" ");
            let Some(upstream_id) = state.proxy.upstream_tokens.insert(upstream) else {
                return token_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error");
            };
            let Some(pair) =
                mint_token_pair(&state.proxy, client_id, &upstream_id, ttl, wants_refresh)
            else {
                // The access-token mint can already have succeeded when the
                // refresh-token mint is what failed; `take_session` cleans
                // up whichever of the two actually landed, not just the
                // upstream set.
                let _ = state.proxy.take_session(&upstream_id);
                return token_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error");
            };
            state.proxy.codes.mark_consumed(
                code,
                pair.access_digest,
                pair.refresh_digest,
                upstream_id,
            );
            token_response(&pair, &granted_scope)
        }
    }
}

/// `grant_type=refresh_token` (R1, R2, R4, R6). Rotates on success: a new
/// access/refresh pair is stored *before* the presented refresh token is
/// retired (risk 1), and upstream is refreshed with the confidential
/// upstream client, never the requesting (public) one.
async fn refresh_token_grant(state: &FlowState, fields: &HashMap<String, String>) -> Response {
    let (Some(refresh_token), Some(requested_client_id)) =
        (fields.get("refresh_token"), fields.get("client_id"))
    else {
        return token_error(StatusCode::BAD_REQUEST, "invalid_request");
    };

    // `_in_flight`, not `_`: the leading underscore only silences the
    // unused-variable lint, the binding still lives until this function
    // returns — that is what keeps the redeemed token locked out of a
    // second, concurrent redemption for the whole refresh (finding 2). A
    // bare `_` would drop the guard immediately and silently reinstate the
    // race.
    let (bound_client_id, upstream_id, _in_flight) =
        match state.proxy.refresh_tokens.redeem(refresh_token) {
            RefreshOutcome::Invalid => {
                return token_error(StatusCode::BAD_REQUEST, "invalid_grant");
            }
            RefreshOutcome::Reused { upstream_id } => {
                // R2: a replayed (already-rotated) refresh token, or a
                // concurrent second use of a still-active one, kills
                // whatever is currently live for its session, however many
                // rotations ago it was current, and — unlike code-replay
                // containment — is revoked upstream too, since a leaked
                // refresh token is a far longer-lived threat than a leaked
                // authorization code.
                if let Some(upstream) = state.proxy.take_session(&upstream_id) {
                    revoke_upstream_and_purge(state, &upstream).await;
                }
                tracing::warn!(
                    "refresh token reuse detected (replay or concurrent use); revoked the session"
                );
                return token_error(StatusCode::BAD_REQUEST, "invalid_grant");
            }
            RefreshOutcome::Ok {
                client_id,
                upstream_id,
                guard,
            } => (client_id, upstream_id, guard),
        };
    if &bound_client_id != requested_client_id {
        // `_in_flight` drops here, restoring the token to `Active`: a
        // wrong `client_id` does not need to lock the token out.
        return token_error(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    // The client binding is now validated, so this use is trusted.
    state.proxy.registry.touch(&bound_client_id);

    let Some(old_refresh_secret) = state.proxy.upstream_tokens.refresh_token(&upstream_id) else {
        // The session died between `redeem` and here (e.g. a concurrent
        // revoke) — clean up this now-orphaned refresh token and fail
        // uniformly (R1).
        state.proxy.refresh_tokens.discard(refresh_token);
        return token_error(StatusCode::BAD_REQUEST, "invalid_grant");
    };
    let old_granted_scopes = state
        .proxy
        .upstream_tokens
        .granted_scopes(&upstream_id)
        .unwrap_or_default();

    let credential = Credential::Basic {
        user: state.upstream_client_id.clone(),
        pass: state.upstream_client_secret.clone(),
    };
    let refreshed = state
        .redmine
        .as_user(&credential)
        .refresh_access_token(&old_refresh_secret)
        .await;

    let new_upstream = match refreshed {
        Err(error) => {
            tracing::warn!(%error, "upstream refresh failed; the session has been cleaned up");
            state.proxy.refresh_tokens.discard(refresh_token);
            if let Some(upstream) = state.proxy.take_session(&upstream_id) {
                state.verifier.purge(&upstream.access);
            }
            return token_error(StatusCode::BAD_REQUEST, "invalid_grant");
        }
        Ok(token) => token,
    };

    let granted_scopes = new_upstream.scope.as_deref().map_or_else(
        || old_granted_scopes.clone(),
        |raw| {
            raw.split_ascii_whitespace()
                .map(ToString::to_string)
                .collect()
        },
    );
    let expires_at = expires_after(Duration::from_secs(new_upstream.expires_in.unwrap_or(3600)));
    let ttl = expires_at.saturating_duration_since(Instant::now());
    let wants_refresh = new_upstream.refresh_token.is_some();
    let granted_scope = granted_scopes.join(" ");
    if let Err(set) = state.proxy.upstream_tokens.replace(
        &upstream_id,
        UpstreamTokenSet {
            access: new_upstream.access_token,
            refresh: new_upstream.refresh_token,
            granted_scopes,
            expires_at,
        },
    ) {
        // The session vanished between `redeem` and here — a concurrent
        // `/revoke`, or a racing redemption's reuse-containment path that
        // won first. `replace` found nothing to update, so the upstream
        // token set this request just obtained is orphaned; revoke it
        // rather than mint anything against a session that no longer
        // exists (finding 2).
        state.proxy.refresh_tokens.discard(refresh_token);
        revoke_upstream_and_purge(state, &set).await;
        tracing::warn!("session vanished mid-refresh; revoked the orphaned upstream token set");
        return token_error(StatusCode::BAD_REQUEST, "invalid_grant");
    }

    let Some(pair) = mint_token_pair(
        &state.proxy,
        &bound_client_id,
        &upstream_id,
        ttl,
        wants_refresh,
    ) else {
        // `_in_flight` restores the token to `Active` on this return, so
        // the client's retry works — and it will present the *new*
        // upstream refresh secret, since `replace` above already
        // succeeded.
        return token_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error");
    };
    state
        .proxy
        .refresh_tokens
        .retire(refresh_token, upstream_id);
    token_response(&pair, &granted_scope)
}

// --- POST /revoke (R5, R6) ---------------------------------------------------

/// The prefixes minted by [`super::store::TokenStore`] and
/// [`super::store::RefreshStore`] respectively (P2).
const ACCESS_TOKEN_PREFIX: &str = "rup_at_";
const REFRESH_TOKEN_PREFIX: &str = "rup_rt_";

/// Revokes `set` upstream with the confidential upstream client credential,
/// then purges its access token from the introspection cache (R6). Prefers
/// the refresh token when present, falling back to the access token
/// otherwise: Doorkeeper revokes both credentials by revoking the single
/// database row either holds (V3), but an already-expired access token is a
/// silent no-op to revoke (V1) while the refresh token never expires on its
/// own (V5) — so the refresh token is the one worth sending whenever there
/// is one. Never both: `token_type_hint` is a one-way filter in the pinned
/// Doorkeeper version (V4), and one call already revokes the row. Logs
/// rather than fails on an upstream error: local state is already gone by
/// the time this runs, and RFC 7009 requires `/revoke` to answer `200`
/// regardless.
async fn revoke_upstream_and_purge(state: &FlowState, set: &UpstreamTokenSet) {
    let credential = Credential::Basic {
        user: state.upstream_client_id.clone(),
        pass: state.upstream_client_secret.clone(),
    };
    let (token, hint) = set
        .refresh
        .as_ref()
        .map_or((&set.access, "access_token"), |refresh| {
            (refresh, "refresh_token")
        });
    if let Err(error) = state
        .redmine
        .as_user(&credential)
        .revoke_token(token, Some(hint))
        .await
    {
        tracing::warn!(%error, "upstream revocation failed; local state was already removed");
    }
    state.verifier.purge(&set.access);
}

/// `POST /revoke` in `oauth-proxy` mode (R5): accepts a `rup_at_`/`rup_rt_`
/// token, deletes the pair and the upstream token set *first* — so our
/// revocation holds even if Redmine is unreachable — then revokes upstream.
/// No client authentication: every client in this mode is public (P8), and
/// possession of the token is the only credential RFC 7009 needs from one.
/// A token that is not ours, or is unrecognised, is a `200` no-op.
async fn revoke(State(state): State<FlowState>, request: Request) -> Response {
    if !content_type_is_form(&request) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported media type: expected application/x-www-form-urlencoded",
        )
            .into_response();
    }

    let Ok(bytes) = axum::body::to_bytes(request.into_body(), MAX_TOKEN_BODY_BYTES).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "payload too large").into_response();
    };

    let fields: HashMap<String, String> = url::form_urlencoded::parse(&bytes)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    let Some(token) = fields.get("token").filter(|t| !t.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({
                "error": "invalid_request",
                "error_description": "token is required",
            })),
        )
            .into_response();
    };

    let upstream = if token.starts_with(ACCESS_TOKEN_PREFIX) {
        state
            .proxy
            .tokens
            .take(token)
            .and_then(|entry| state.proxy.take_session(&entry.upstream_id))
    } else if token.starts_with(REFRESH_TOKEN_PREFIX) {
        state
            .proxy
            .refresh_tokens
            .take(token)
            .and_then(|(_client_id, upstream_id)| state.proxy.take_session(&upstream_id))
    } else {
        None
    };

    if let Some(upstream) = upstream {
        revoke_upstream_and_purge(&state, &upstream).await;
    }

    StatusCode::OK.into_response()
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
            proxy: Arc::new(ProxyState::new()),
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
            proxy: Arc::new(ProxyState::new()),
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

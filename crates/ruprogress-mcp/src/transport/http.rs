//! Streamable HTTP transport.
//!
//! The edge checks that matter for this transport — `Host` allowlisting
//! (DNS rebinding), `Origin` allowlisting, and the request-body cap — are
//! implemented inside rmcp's `StreamableHttpService` and enforced there, so
//! this module configures them rather than reimplementing them. Duplicating
//! them in a tower layer would only produce a second rejection path with a
//! different status code and a different log line.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Once};
use std::time::Instant;

use anyhow::Context as _;
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path as AxumPath, Request, State};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use redmine_client::{Credential, RedmineClient};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::json;
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use url::Url;
use uuid::Uuid;

use crate::attachments::AttachmentStore;
use crate::auth::oauth::{self, TokenVerifier};
use crate::auth::proxy::{self as auth_proxy, ProxyAuthState};
use crate::config::{AuthMode, Config, DiscoveryAs, HttpConfig, OAuthConfig, OAuthProxyConfig};
use crate::health::{self, HealthState};
use crate::oauth as oauth_docs;
use crate::oauth::metadata::DiscoveryMode;
use crate::ratelimit::{self, Limiter};
use crate::server::RedmineMcp;

/// The session manager in use. Swapping this for
/// `session::local::LocalSessionManager` plus `with_legacy_session_mode(true)`
/// is the whole change needed to go back to stateful sessions, should a client
/// turn up that hard-requires an `Mcp-Session-Id`.
type SessionManager = NeverSessionManager;

// --- Rate limiting (phase 9.2) ---------------------------------------------

/// A rate-limit bucket key. `Fallback` is reserved for the programming-error
/// case where `ConnectInfo` is unexpectedly absent (RL10) — never a real
/// client's key, so it cannot collide with one.
#[derive(Clone, PartialEq, Eq, Hash)]
enum LimitKey {
    Ip(IpAddr),
    TokenDigest([u8; 32]),
    Fallback,
}

fn connect_info_ip(req: &Request) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip())
}

/// RL10: a missing `ConnectInfo` at request time means the HTTP server was
/// not started via `into_make_service_with_connect_info` — a programming
/// error, not a runtime condition, so it is logged loudly but only once per
/// process rather than once per request.
fn log_missing_connect_info_once() {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
        tracing::error!(
            "the rate limiter could not read the peer address (ConnectInfo is missing from the \
             request); the HTTP server must be started via \
             into_make_service_with_connect_info for rate limiting to key correctly"
        );
    });
}

/// The standard class's key (RL5): a bearer token's digest when a
/// well-formed one is present — so one NAT or containerised client does not
/// share a bucket across distinct callers — otherwise the peer IP.
fn standard_key(req: &Request) -> LimitKey {
    if let Ok(token) = oauth::extract_bearer(req.headers()) {
        return LimitKey::TokenDigest(TokenVerifier::digest(token.expose_secret()));
    }
    if let Some(ip) = connect_info_ip(req) {
        return LimitKey::Ip(ip);
    }
    log_missing_connect_info_once();
    LimitKey::Fallback
}

/// The strict class's key (RL3): peer IP only, never a header. `None` when
/// `ConnectInfo` is absent — RL10 fails this class closed rather than
/// falling back to a shared bucket the way the standard class does.
fn strict_key(req: &Request) -> Option<LimitKey> {
    if let Some(ip) = connect_info_ip(req) {
        return Some(LimitKey::Ip(ip));
    }
    log_missing_connect_info_once();
    None
}

/// `429` with `Retry-After` and `Cache-Control: no-store` (RL8) — never a
/// JSON-RPC error: at limiter time there may be no parsed envelope and no
/// session to attach one to.
fn too_many_requests(retry_after_secs: u64) -> Response {
    let mut response = (
        http::StatusCode::TOO_MANY_REQUESTS,
        axum::Json(json!({ "error": "rate_limited" })),
    )
        .into_response();
    let headers = response.headers_mut();
    headers.insert(
        http::header::RETRY_AFTER,
        http::HeaderValue::from_str(&retry_after_secs.to_string())
            .unwrap_or_else(|_| http::HeaderValue::from_static("1")),
    );
    headers.insert(
        http::header::CACHE_CONTROL,
        http::HeaderValue::from_static("no-store"),
    );
    response
}

/// Turns a `Limiter` decision into either a `429` or "let it through",
/// logging a transition (RL11) rather than a line per request.
fn apply_decision(class: &'static str, decision: ratelimit::Decision) -> Option<Response> {
    match decision {
        ratelimit::Decision::Allow { recovered } => {
            if recovered {
                tracing::info!(class, "rate limit recovered for a key");
            }
            None
        }
        ratelimit::Decision::Deny {
            retry_after_secs,
            newly_limited,
        } => {
            if newly_limited {
                tracing::warn!(class, retry_after_secs, "rate limit engaged for a key");
            }
            Some(too_many_requests(retry_after_secs))
        }
    }
}

/// RL4: the standard class, mounted on `/mcp` and `/files/{uuid}`.
async fn rate_limit_standard(
    State(limiter): State<Arc<Limiter<LimitKey>>>,
    req: Request,
    next: Next,
) -> Response {
    let decision = limiter.check(standard_key(&req), Instant::now());
    match apply_decision("standard", decision) {
        Some(response) => response,
        None => next.run(req).await,
    }
}

/// RL4: the strict class, mounted on the unauthenticated, state-allocating
/// oauth-proxy endpoints (`/register`, `/authorize`, `/auth/callback`,
/// `/token`, `/revoke`).
async fn rate_limit_strict(
    State(limiter): State<Arc<Limiter<LimitKey>>>,
    req: Request,
    next: Next,
) -> Response {
    let Some(key) = strict_key(&req) else {
        // RL10: fail closed rather than share one bucket across every
        // caller when the peer address could not be determined.
        return too_many_requests(1);
    };
    let decision = limiter.check(key, Instant::now());
    match apply_decision("strict", decision) {
        Some(response) => response,
        None => next.run(req).await,
    }
}

/// One class's limiter, shared (cheaply cloned) across every route it is
/// layered onto.
type SharedLimiter = Arc<Limiter<LimitKey>>;

/// RL9: `false` restores pre-9.2 behaviour exactly — no `Limiter` is
/// constructed, and `router` layers no rate-limit middleware at all. Split
/// out of `router` to keep it under clippy's line-count pedantic threshold.
fn build_limiters(cfg: &HttpConfig) -> (Option<SharedLimiter>, Option<SharedLimiter>) {
    if !cfg.rate_limit.enabled {
        return (None, None);
    }
    let standard = Arc::new(Limiter::new(
        cfg.rate_limit.standard_rps,
        cfg.rate_limit.standard_burst,
        cfg.rate_limit.max_keys,
    ));
    let strict = Arc::new(Limiter::new(
        cfg.rate_limit.strict_rps,
        cfg.rate_limit.strict_burst,
        cfg.rate_limit.max_keys,
    ));
    (Some(standard), Some(strict))
}

/// Layers the standard class's rate-limit middleware onto `route` when
/// enabled (RL9), otherwise returns it unchanged. Shared by `router` (the
/// MCP route) and `files_route`.
fn layer_standard_class(route: Router, limiter: Option<&SharedLimiter>) -> Router {
    match limiter {
        Some(limiter) => route.layer(middleware::from_fn_with_state(
            limiter.clone(),
            rate_limit_standard,
        )),
        None => route,
    }
}

/// Layers the strict class's rate-limit middleware onto `route` when
/// enabled (RL9), otherwise returns it unchanged. Used only for the
/// oauth-proxy flow routes (`/register`&c.).
fn layer_strict_class(route: Router, limiter: Option<&SharedLimiter>) -> Router {
    match limiter {
        Some(limiter) => route.layer(middleware::from_fn_with_state(
            limiter.clone(),
            rate_limit_strict,
        )),
        None => route,
    }
}

/// `/livez`, `/readyz`, and `/health` (an alias for `/readyz`). Never rate
/// limited (RL6) and never traced (see the comment above `router`'s
/// `mcp_route` construction) — split out of `router` to keep it under
/// clippy's line-count pedantic threshold.
fn health_routes(cfg: &HttpConfig, health_state: HealthState) -> Router {
    Router::new()
        .route("/livez", get(health::livez))
        .route("/readyz", get(health::readyz))
        // Alias, so an `.env` or compose file written for the reference
        // server keeps working. It maps to readiness, not liveness.
        .route("/health", get(health::readyz))
        .layer(TimeoutLayer::with_status_code(
            http::StatusCode::GATEWAY_TIMEOUT,
            cfg.request_timeout,
        ))
        // Outside the timeout layer, so the 504 it synthesises is covered too.
        .layer(SetResponseHeaderLayer::overriding(
            http::header::CACHE_CONTROL,
            http::HeaderValue::from_static("no-store"),
        ))
        .with_state(health_state)
}

/// Build the full HTTP router.
///
/// Takes no listener so that tests can bind `127.0.0.1:0` themselves and learn
/// the real port.
///
/// `service_ct` **aborts in-flight tool calls** — rmcp races it against the
/// handler's first message (`tower.rs:1228`) and turns a loser into a 500. It
/// is therefore not the shutdown signal; see [`serve`].
pub fn router(server: RedmineMcp, cfg: &HttpConfig, service_ct: CancellationToken) -> Router {
    let service_config = StreamableHttpServerConfig::default()
        // Stateless: no in-memory session state, plain `application/json`
        // responses, and no `Mcp-Session-Id` to expose through CORS.
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_cancellation_token(service_ct)
        // Never `disable_allowed_hosts()`: an empty list means *allow every
        // host* in rmcp, which is the opposite of what the name suggests.
        // `HttpConfig` only ever produces an empty list when the operator set
        // `REDMINE_MCP_ALLOWED_HOSTS=*` explicitly.
        .with_allowed_hosts(cfg.allowed_hosts.clone())
        .with_allowed_origins(cfg.allowed_origins.clone())
        .with_max_request_body_bytes(cfg.max_request_body_bytes);

    let attachments = server.attachments();
    let health_state = HealthState::new(server.clone(), cfg.health_ttl);
    let (standard_limiter, strict_limiter) = build_limiters(cfg);
    // Read before `server` moves into `mcp_service` below. `discovery_mode`
    // and `verifier()` are set together by every mode that has either (see
    // `RedmineMcp::new`), so zipping them here is what actually establishes
    // "discovery exists" — no downstream site needs to re-derive it from
    // `AuthMode` again.
    let discovery_mode = DiscoveryMode::from_auth(&server.inner.config.auth).map(|(mode, _)| mode);
    // Cloned/read once here, before `server` moves into `mcp_service` below.
    let (proxy_config, proxy_state) = proxy_mode_state(&server);
    let redmine_base = server.inner.config.redmine.url.clone();
    let discovery = discovery_mode
        .zip(server.inner.config.oauth_resource().cloned())
        .zip(server.verifier())
        .map(|((mode, oauth), verifier)| {
            (
                server.inner.config.clone(),
                server.client(),
                verifier,
                oauth,
                mode,
            )
        });
    let mcp_service: StreamableHttpService<RedmineMcp, SessionManager> = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(SessionManager::default()),
        service_config,
    );

    let health_routes = health_routes(cfg, health_state);

    // Tracing is scoped to the MCP route rather than applied to the whole
    // router, because the health routes must not be traced at all: probes run
    // on a timer, and `TraceLayer` emits an ERROR for every 5xx — so a Redmine
    // outage would turn a `/readyz` poll into a flood. Suppressing only the
    // span (`Span::none()`) is not enough; the response events fire anyway.
    let mut mcp_route = Router::new()
        // `nest_service`, not `nest`: `nest` can drop the `Host` header hyper
        // synthesises from an HTTP/2 `:authority`, which is the input rmcp's
        // rebinding check reads.
        .nest_service(&cfg.mcp_path, mcp_service);

    // SECURITY: mounted on the MCP route only, and only in a bearer-token
    // auth mode. Every other route this router serves — `/livez`, `/readyz`,
    // `/health`, `/files/{uuid}` (an unguessable, TTL-bounded capability
    // URL), `/register`, `/authorize`, `/auth/callback`, and every
    // `/.well-known/*` discovery document — must stay reachable with no
    // bearer token: RFC 9728 metadata has to be fetchable *before* a client
    // has a token, and probes must not need a credential (O8).
    let mut proxy_flow_routes: Option<Router> = None;
    match &discovery {
        Some((_, _, verifier, oauth, DiscoveryMode::Oauth)) => {
            let challenge = Arc::new(oauth::Challenge::build(&oauth.base_url, &cfg.mcp_path));
            mcp_route = mcp_route.layer(middleware::from_fn_with_state(
                (verifier.clone(), challenge),
                oauth::require_bearer,
            ));
        }
        Some((_, client, verifier, oauth, DiscoveryMode::OAuthProxy)) => {
            // `proxy_config` and `discovery` are both derived from
            // `server.inner.config.auth` before `server` moved into
            // `mcp_service` above, so they always agree; a graceful skip
            // (never a panic) is the fail-closed response to that
            // invariant somehow not holding.
            if let (Some(proxy), Some(proxy_state)) = (proxy_config.as_ref(), proxy_state.as_ref())
            {
                let (route, flow) = mount_oauth_proxy(
                    mcp_route,
                    proxy,
                    oauth,
                    verifier,
                    client,
                    &redmine_base,
                    &cfg.mcp_path,
                    proxy_state,
                );
                mcp_route = route;
                proxy_flow_routes = Some(flow);
            } else {
                tracing::error!(
                    "oauth-proxy discovery mode selected without an OAuthProxyConfig/ProxyState; \
                     refusing to mount its routes"
                );
            }
        }
        None => {}
    }

    // RL4/RL10: outside the auth layer added above (an unauthenticated
    // flood must be rejected before it costs an introspection call), inside
    // `TraceLayer` and the CORS layer added further below.
    mcp_route = layer_standard_class(mcp_route, standard_limiter.as_ref());

    let mcp_route = mcp_route.layer(TraceLayer::new_for_http());

    let mut router = Router::new()
        .merge(mcp_route)
        .merge(health_routes)
        .merge(files_route(
            attachments,
            cfg.allowed_hosts.clone(),
            standard_limiter.as_ref(),
        ));

    // Merged outside `mcp_route`, so the bearer-auth layer above never sees
    // these: RFC 9728/8414 metadata must be reachable with no token (O8,
    // D6), and `/revoke` (`oauth` mode only for now — `oauth-proxy` needs
    // mode-specific semantics of its own before it can be mounted there)
    // likewise authenticates its own caller rather than needing one.
    if let Some((config, client, verifier, oauth, mode)) = discovery {
        router = router.merge(well_known_routes(config, oauth, &cfg.mcp_path, mode));
        if mode == DiscoveryMode::Oauth {
            router = router.merge(revoke_route(client, verifier));
        }
    }
    if let Some(proxy_flow_routes) = proxy_flow_routes {
        // RL4: strict class — `/register`, `/authorize`, `/auth/callback`,
        // `/token`, and `/revoke` all allocate state or spend an
        // unauthenticated caller's request on a fixed cost.
        let proxy_flow_routes = layer_strict_class(proxy_flow_routes, strict_limiter.as_ref());
        router = router.merge(proxy_flow_routes);
    }

    if let Some(cors) = cors_layer(cfg) {
        router = router.layer(cors);
    }

    router.layer(SetResponseHeaderLayer::overriding(
        http::header::X_CONTENT_TYPE_OPTIONS,
        http::HeaderValue::from_static("nosniff"),
    ))
}

/// `AuthMode::OAuthProxy`'s config, and the store bundle `RedmineMcp::new`
/// built alongside `oauth_verifier` (R7) — shared with
/// `tools::meta::get_mcp_server_info` rather than a second, router-local
/// set of stores it could never see. The upstream client, redirect policy,
/// etc. live only on `OAuthProxyConfig`, not the shared `OAuthConfig`
/// `DiscoveryMode::from_auth` returns, so proxy mode needs its own look.
fn proxy_mode_state(
    server: &RedmineMcp,
) -> (
    Option<OAuthProxyConfig>,
    Option<Arc<oauth_docs::proxy::store::ProxyState>>,
) {
    let proxy_config = match &server.inner.config.auth {
        AuthMode::OAuthProxy(proxy) => Some(proxy.clone()),
        AuthMode::Legacy { .. } | AuthMode::LegacyPerUser { .. } | AuthMode::OAuth(_) => None,
    };
    (proxy_config, server.oauth_proxy_state())
}

/// Layers the proxy-bearer middleware onto `mcp_route` and returns it
/// alongside the stand-alone router for `/register`, `/authorize`,
/// `/auth/callback`, `/token`, and `/revoke` — all sharing `proxy_state`,
/// the same store bundle `get_mcp_server_info` reads its session counts
/// from (R7). Split out of `router` to keep that function under clippy's
/// line-count pedantic threshold.
#[allow(clippy::too_many_arguments)]
fn mount_oauth_proxy(
    mcp_route: Router,
    proxy: &OAuthProxyConfig,
    oauth: &OAuthConfig,
    verifier: &Arc<TokenVerifier>,
    client: &RedmineClient,
    redmine_base: &Url,
    mcp_path: &str,
    proxy_state: &Arc<oauth_docs::proxy::store::ProxyState>,
) -> (Router, Router) {
    let challenge = Arc::new(oauth::Challenge::build(&oauth.base_url, mcp_path));
    let redirects = Arc::new(proxy.redirects.clone());

    // P9: a token-store lookup ahead of the same `TokenVerifier` `oauth`
    // mode uses — never a fallback to accepting a raw upstream token here.
    let mcp_route = mcp_route.layer(middleware::from_fn_with_state(
        ProxyAuthState {
            proxy: proxy_state.clone(),
            verifier: verifier.clone(),
            challenge,
        },
        auth_proxy::require_proxy_bearer,
    ));

    let flow_routes = Router::new()
        .merge(oauth_docs::proxy::endpoints::register_route(
            proxy_state.clone(),
            redirects.clone(),
        ))
        .merge(oauth_docs::proxy::endpoints::flow_routes(
            proxy_state.clone(),
            redirects,
            Arc::new(oauth.clone()),
            redmine_base.clone(),
            proxy.upstream_client_id.clone(),
            proxy.upstream_client_secret.clone(),
            client.clone(),
            verifier.clone(),
        ));

    (mcp_route, flow_routes)
}

#[derive(Clone)]
struct DiscoveryState {
    config: Arc<Config>,
    oauth: Arc<OAuthConfig>,
    mcp_path: Arc<str>,
    mode: DiscoveryMode,
}

async fn protected_resource_doc(State(state): State<DiscoveryState>) -> Response {
    axum::Json(oauth_docs::metadata::protected_resource(
        &state.config,
        &state.oauth,
        &state.mcp_path,
    ))
    .into_response()
}

async fn authorization_server_doc(State(state): State<DiscoveryState>) -> Response {
    axum::Json(oauth_docs::metadata::authorization_server(
        &state.config,
        &state.oauth,
        state.mode,
    ))
    .into_response()
}

/// RFC 9728 protected-resource and RFC 8414 authorization-server metadata
/// (D3, D6): unauthenticated, rendered per request rather than memoised
/// (D8), and cached briefly at the edge so a client's own startup burst
/// does not re-render them on every request.
///
/// `oauth`/`mode` are supplied by the caller (`router`, above) rather than
/// re-derived from `config.auth` — this function has no way to fall back if
/// they were absent. In `oauth-proxy` mode `oauth.discovery_as` is always
/// `SelfHosted` (P12, enforced at config-parse time), so the AS document
/// lands at the root well-known path exactly as `self` discovery mode does
/// in `oauth` — no extra branch needed here for *where* it is served, only
/// for *what* `authorization_server_doc` renders.
fn well_known_routes(
    config: Config,
    oauth: OAuthConfig,
    mcp_path: &str,
    mode: DiscoveryMode,
) -> Router {
    let protected_resource_path = format!("/.well-known/oauth-protected-resource{mcp_path}");
    // D3: the suffixed path serves the AS document in `redmine` mode; `self`
    // mode moves it to the root well-known location instead, so the
    // suffixed path 404s there (never registered) rather than serving a
    // second document with a different issuer.
    let as_path = match oauth.discovery_as {
        DiscoveryAs::Redmine => format!("/.well-known/oauth-authorization-server{mcp_path}"),
        DiscoveryAs::SelfHosted => "/.well-known/oauth-authorization-server".to_string(),
    };
    Router::new()
        .route(&protected_resource_path, get(protected_resource_doc))
        .route(&as_path, get(authorization_server_doc))
        .layer(SetResponseHeaderLayer::overriding(
            http::header::CACHE_CONTROL,
            http::HeaderValue::from_static("public, max-age=300"),
        ))
        .with_state(DiscoveryState {
            config: Arc::new(config),
            oauth: Arc::new(oauth),
            mcp_path: Arc::from(mcp_path),
            mode,
        })
}

/// Bytes cap for `POST /revoke`'s request body (D4): far above any real RFC
/// 7009 form (`token`, `token_type_hint`, and client credentials), far below
/// anything worth reading in full before rejecting.
const MAX_REVOKE_BODY_BYTES: usize = 8 * 1024;

#[derive(Clone)]
struct RevokeState {
    client: RedmineClient,
    verifier: Arc<TokenVerifier>,
}

/// `POST /revoke` (RFC 7009, D4): a narrow proxy to Redmine's own
/// `/oauth/revoke`. Unauthenticated at our edge — Redmine authenticates the
/// client — but never a general-purpose forwarder: only `token`/
/// `token_type_hint` and the caller's own client authentication ever leave
/// this handler, and everything else in the request body is dropped.
///
/// Only ever mounted in `AuthMode::OAuth` — see `router`. A candidate for
/// a future rate limiter: an unauthenticated route that makes one
/// upstream request per call.
fn revoke_route(client: RedmineClient, verifier: Arc<TokenVerifier>) -> Router {
    Router::new()
        .route("/revoke", post(revoke))
        .with_state(RevokeState { client, verifier })
}

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

/// The caller's own client authentication (D4): an `Authorization: Basic`
/// header takes precedence; otherwise `client_id`/`client_secret` form
/// fields. Never this server's own introspection credential — accepting
/// that here would let any caller revoke any token by riding on our
/// client's identity.
fn client_credential(
    auth_header: Option<&http::HeaderValue>,
    fields: &HashMap<String, String>,
) -> Option<Credential> {
    if let Some(value) = auth_header {
        let text = value.to_str().ok()?;
        let encoded = text.strip_prefix("Basic ")?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let decoded = String::from_utf8(decoded).ok()?;
        let (user, pass) = decoded.split_once(':')?;
        return Some(Credential::Basic {
            user: user.to_string(),
            pass: SecretString::from(pass.to_string()),
        });
    }
    let client_id = fields.get("client_id")?;
    let client_secret = fields.get("client_secret")?;
    Some(Credential::Basic {
        user: client_id.clone(),
        pass: SecretString::from(client_secret.clone()),
    })
}

async fn revoke(State(state): State<RevokeState>, request: Request) -> Response {
    if !content_type_is_form(&request) {
        return (
            http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported media type: expected application/x-www-form-urlencoded",
        )
            .into_response();
    }
    let auth_header = request.headers().get(http::header::AUTHORIZATION).cloned();

    let Ok(bytes) = axum::body::to_bytes(request.into_body(), MAX_REVOKE_BODY_BYTES).await else {
        return (http::StatusCode::PAYLOAD_TOO_LARGE, "payload too large").into_response();
    };

    let fields: HashMap<String, String> = url::form_urlencoded::parse(&bytes)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    let Some(token) = fields.get("token").filter(|t| !t.is_empty()) else {
        return (
            http::StatusCode::BAD_REQUEST,
            axum::Json(json!({
                "error": "invalid_request",
                "error_description": "token is required",
            })),
        )
            .into_response();
    };
    let token_type_hint = fields.get("token_type_hint").map(String::as_str);

    let Some(credential) = client_credential(auth_header.as_ref(), &fields) else {
        return (
            http::StatusCode::BAD_REQUEST,
            axum::Json(json!({
                "error": "invalid_client",
                "error_description": "client authentication is required",
            })),
        )
            .into_response();
    };

    let token = SecretString::from(token.clone());
    let scoped = state.client.as_user_owned(credential);
    match scoped.revoke_token(&token, token_type_hint).await {
        Ok(()) => {
            state.verifier.purge(&token);
            http::StatusCode::OK.into_response()
        }
        Err(error) => {
            let status = error.status().unwrap_or(http::StatusCode::BAD_GATEWAY);
            tracing::warn!(%error, "revocation request rejected upstream");
            (status, axum::Json(json!({ "error": "invalid_client" }))).into_response()
        }
    }
}

/// Exact-match CORS, or nothing at all when no origins are configured.
///
/// Never `AllowOrigin::mirror_request` and never `allow_credentials`: this
/// server holds a Redmine credential, so reflecting arbitrary origins would
/// turn any page a user visits into a Redmine client.
fn cors_layer(cfg: &HttpConfig) -> Option<CorsLayer> {
    let origins: Vec<http::HeaderValue> = cfg
        .allowed_origins
        .iter()
        .filter_map(|origin| http::HeaderValue::from_str(origin).ok())
        .collect();
    if origins.is_empty() {
        return None;
    }
    Some(
        CorsLayer::new()
            .allow_origin(origins)
            // GET and DELETE on the MCP route are 405 in stateless mode.
            .allow_methods([http::Method::POST, http::Method::OPTIONS])
            .allow_headers([
                http::header::CONTENT_TYPE,
                http::header::AUTHORIZATION,
                http::HeaderName::from_static("mcp-protocol-version"),
            ])
            .expose_headers([http::HeaderName::from_static("mcp-protocol-version")]),
    )
}

/// `GET /files/{uuid}`: serves a stored attachment (a separate tool
/// populates the store; this route just serves what is there).
///
/// Reuses `HttpConfig::allowed_hosts` for a `Host` allowlist check: rmcp's
/// own `Host` check runs only inside `StreamableHttpService`, so a route
/// mounted ourselves needs its own copy of the same check, not a weaker one.
///
/// SECURITY: this route checks no Redmine credential in any auth mode,
/// including `legacy-per-user`. The UUID is an unguessable, TTL-bounded
/// bearer capability, and the route exists precisely so a browser or other
/// non-MCP HTTP client can fetch the bytes from a plain URL with no Redmine
/// credential of its own — binding it to a fetching request's
/// `X-Redmine-API-Key` would defeat that use case, and disabling it would
/// break `get_redmine_attachment`. See `docs/legacy-per-user-auth.md`.
fn files_route(
    store: Arc<AttachmentStore>,
    allowed_hosts: Vec<String>,
    limiter: Option<&SharedLimiter>,
) -> Router {
    let router = Router::new()
        .route("/files/{uuid}", get(serve_file))
        .layer(middleware::from_fn(move |req: Request, next: Next| {
            let allowed_hosts = allowed_hosts.clone();
            async move {
                match validate_files_host(&req, &allowed_hosts) {
                    Ok(()) => next.run(req).await,
                    Err((status, message)) => (status, message).into_response(),
                }
            }
        }))
        .layer(SetResponseHeaderLayer::overriding(
            http::header::CACHE_CONTROL,
            http::HeaderValue::from_static("no-store"),
        ))
        .with_state(store);
    // RL4: the standard class, shared with `/mcp`.
    layer_standard_class(router, limiter)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestAuthority {
    host: String,
    port: Option<u16>,
}

fn normalize_host(host: &str) -> String {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase()
}

fn parse_authority(raw: &str) -> Option<RequestAuthority> {
    let authority = http::uri::Authority::try_from(raw.trim()).ok()?;
    Some(RequestAuthority {
        host: normalize_host(authority.host()),
        port: authority.port_u16(),
    })
}

fn request_authority(req: &Request) -> Option<RequestAuthority> {
    if let Some(host) = req.headers().get(http::header::HOST) {
        return parse_authority(host.to_str().ok()?);
    }
    // HTTP/2 carries the host in `:authority`, which some middleware can
    // separate from an explicit `Host` header — same fallback rmcp uses.
    let authority = req.uri().authority()?;
    Some(RequestAuthority {
        host: normalize_host(authority.host()),
        port: authority.port_u16(),
    })
}

fn host_is_allowed(host: &RequestAuthority, allowed_hosts: &[String]) -> bool {
    if allowed_hosts.is_empty() {
        return true;
    }
    allowed_hosts
        .iter()
        .filter_map(|raw| parse_authority(raw))
        .any(|allowed| {
            allowed.host == host.host
                && match allowed.port {
                    Some(port) => host.port == Some(port),
                    None => true,
                }
        })
}

/// `Err` carries a status and a static message rather than a built
/// `Response`, which would make this `Result`'s error variant large enough
/// to trip `clippy::result_large_err` for no benefit — the caller builds the
/// response at the one call site that needs one.
fn validate_files_host(
    req: &Request,
    allowed_hosts: &[String],
) -> Result<(), (http::StatusCode, &'static str)> {
    let Some(authority) = request_authority(req) else {
        return Err((
            http::StatusCode::BAD_REQUEST,
            "Bad Request: missing or invalid Host header",
        ));
    };
    if host_is_allowed(&authority, allowed_hosts) {
        Ok(())
    } else {
        tracing::warn!(
            host = ?authority,
            "rejected /files request with disallowed Host header"
        );
        Err((
            http::StatusCode::FORBIDDEN,
            "Forbidden: Host header is not allowed",
        ))
    }
}

/// Percent-encodes everything outside the RFC 5987 `attr-char` set
/// (unreserved characters only), for the `filename*=UTF-8''...` parameter.
/// Hand-rolled rather than pulling in a new dependency for one header value.
fn percent_encode_attr_char(input: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(*byte));
            }
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// Builds a `Content-Disposition: attachment` header value with both a
/// quoted-ASCII fallback and an RFC 5987 `filename*` parameter, since the
/// filename came from Redmine (via [`AttachmentStore::reserve`]'s
/// sanitisation, which permits non-ASCII).
fn content_disposition(filename: &str) -> http::HeaderValue {
    let ascii_fallback: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let ascii_fallback = if ascii_fallback.is_empty() {
        "attachment".to_string()
    } else {
        ascii_fallback
    };
    let encoded = percent_encode_attr_char(filename);
    let value = format!("attachment; filename=\"{ascii_fallback}\"; filename*=UTF-8''{encoded}");
    http::HeaderValue::from_str(&value)
        .unwrap_or_else(|_| http::HeaderValue::from_static("attachment"))
}

/// A `Content-Type` built from Redmine-supplied data must be validated as a
/// well-formed header value before use: Redmine's `content_type` is
/// attacker-influenced, and a value containing e.g. embedded CRLF must not
/// reach the response.
fn content_type_header(content_type: Option<&str>) -> http::HeaderValue {
    content_type
        .and_then(|ct| http::HeaderValue::from_str(ct).ok())
        .unwrap_or_else(|| http::HeaderValue::from_static("application/octet-stream"))
}

async fn serve_file(
    State(store): State<Arc<AttachmentStore>>,
    AxumPath(uuid): AxumPath<Uuid>,
) -> Response {
    let Some(stored) = store.get(uuid).await else {
        return (http::StatusCode::NOT_FOUND, "not found").into_response();
    };
    let file = match tokio::fs::File::open(&stored.path).await {
        Ok(file) => file,
        Err(error) => {
            tracing::error!(%error, %uuid, "failed to open a stored attachment file");
            return (http::StatusCode::NOT_FOUND, "not found").into_response();
        }
    };
    let body = Body::from_stream(ReaderStream::new(file));

    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(
        http::header::CONTENT_TYPE,
        content_type_header(stored.content_type.as_deref()),
    );
    headers.insert(
        http::header::CONTENT_LENGTH,
        http::HeaderValue::from_str(&stored.size.to_string())
            .unwrap_or_else(|_| http::HeaderValue::from_static("0")),
    );
    headers.insert(
        http::header::CONTENT_DISPOSITION,
        content_disposition(&stored.filename),
    );
    response
}

/// Bind and serve until `shutdown` is cancelled, then let in-flight requests
/// finish.
///
/// `shutdown` is deliberately *not* the token handed to rmcp: that one aborts
/// running tool calls, so sharing them would turn every in-flight request into
/// a 500 at the exact moment we claim to be draining. rmcp's token is created
/// here and cancelled only once axum has finished draining, by which point it
/// has nothing left to abort. Bounding how long the drain may take is the
/// caller's job — `axum::serve` waits indefinitely.
///
/// # Errors
///
/// Fails if the configured address cannot be bound or the server task errors.
pub async fn serve(
    server: RedmineMcp,
    cfg: &HttpConfig,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(cfg.bind)
        .await
        .with_context(|| format!("failed to bind {}", cfg.bind))?;
    let local_addr = listener
        .local_addr()
        .with_context(|| format!("failed to read the local address of {}", cfg.bind))?;

    tracing::info!(
        address = %local_addr,
        mcp_path = %cfg.mcp_path,
        // Logged so a 403 can be diagnosed from the boot line alone, without
        // reconstructing the derivation from four environment variables.
        allowed_hosts = ?cfg.allowed_hosts,
        allowed_origins = ?cfg.allowed_origins,
        "serving MCP over streamable HTTP"
    );

    let service_ct = CancellationToken::new();
    let router = router(server, cfg, service_ct.clone());
    // RL10: the rate limiter keys by peer address (RL3), which requires
    // `ConnectInfo` — plain `axum::serve(listener, router)` never populates
    // it.
    let result = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move { shutdown.cancelled_owned().await })
    .await;
    service_ct.cancel();
    result.context("the HTTP server exited with an error")
}

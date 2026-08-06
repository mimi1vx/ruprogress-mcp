//! Streamable HTTP transport.
//!
//! The edge checks that matter for this transport — `Host` allowlisting
//! (DNS rebinding), `Origin` allowlisting, and the request-body cap — are
//! implemented inside rmcp's `StreamableHttpService` and enforced there, so
//! this module configures them rather than reimplementing them. Duplicating
//! them in a tower layer would only produce a second rejection path with a
//! different status code and a different log line.

use std::sync::Arc;

use anyhow::Context as _;
use axum::Router;
use axum::routing::get;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::config::HttpConfig;
use crate::health::{self, HealthState};
use crate::server::RedmineMcp;

/// The session manager in use. Swapping this for
/// `session::local::LocalSessionManager` plus `with_legacy_session_mode(true)`
/// is the whole change needed to go back to stateful sessions, should a client
/// turn up that hard-requires an `Mcp-Session-Id`.
type SessionManager = NeverSessionManager;

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

    let health_state = HealthState::new(server.clone(), cfg.health_ttl);
    let mcp_service: StreamableHttpService<RedmineMcp, SessionManager> = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(SessionManager::default()),
        service_config,
    );

    let health_routes = Router::new()
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
        .with_state(health_state);

    // Tracing is scoped to the MCP route rather than applied to the whole
    // router, because the health routes must not be traced at all: probes run
    // on a timer, and `TraceLayer` emits an ERROR for every 5xx — so a Redmine
    // outage would turn a `/readyz` poll into a flood. Suppressing only the
    // span (`Span::none()`) is not enough; the response events fire anyway.
    let mcp_route = Router::new()
        // `nest_service`, not `nest`: `nest` can drop the `Host` header hyper
        // synthesises from an HTTP/2 `:authority`, which is the input rmcp's
        // rebinding check reads.
        .nest_service(&cfg.mcp_path, mcp_service)
        .layer(TraceLayer::new_for_http());

    let mut router = Router::new().merge(mcp_route).merge(health_routes);

    if let Some(cors) = cors_layer(cfg) {
        router = router.layer(cors);
    }

    router.layer(SetResponseHeaderLayer::overriding(
        http::header::X_CONTENT_TYPE_OPTIONS,
        http::HeaderValue::from_static("nosniff"),
    ))
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
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(async move { shutdown.cancelled_owned().await })
        .await;
    service_ct.cancel();
    result.context("the HTTP server exited with an error")
}

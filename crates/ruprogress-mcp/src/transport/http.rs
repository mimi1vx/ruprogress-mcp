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
use axum::body::Body;
use axum::extract::{Path as AxumPath, Request, State};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use crate::attachments::AttachmentStore;
use crate::auth::oauth;
use crate::config::{AuthMode, HttpConfig};
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

    let attachments = server.attachments();
    let health_state = HealthState::new(server.clone(), cfg.health_ttl);
    let oauth_state = server.verifier().map(|verifier| {
        let AuthMode::OAuth(oauth_config) = &server.inner.config.auth else {
            unreachable!("verifier() is Some only in AuthMode::OAuth");
        };
        let challenge = Arc::new(oauth::Challenge::build(
            &oauth_config.base_url,
            &cfg.mcp_path,
        ));
        (verifier, challenge)
    });
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
    let mut mcp_route = Router::new()
        // `nest_service`, not `nest`: `nest` can drop the `Host` header hyper
        // synthesises from an HTTP/2 `:authority`, which is the input rmcp's
        // rebinding check reads.
        .nest_service(&cfg.mcp_path, mcp_service);

    // SECURITY: mounted on the MCP route only, and only in `oauth` mode.
    // Every other route this router serves — `/livez`, `/readyz`, `/health`,
    // `/files/{uuid}` (6a's L8 capability URL), and every future
    // `/.well-known/*` discovery document — must stay reachable with no
    // bearer token: RFC 9728 metadata has to be fetchable *before* a client
    // has a token, and probes must not need a credential (O8).
    if let Some(state) = oauth_state {
        mcp_route = mcp_route.layer(middleware::from_fn_with_state(state, oauth::require_bearer));
    }

    let mcp_route = mcp_route.layer(TraceLayer::new_for_http());

    let mut router = Router::new()
        .merge(mcp_route)
        .merge(health_routes)
        .merge(files_route(attachments, cfg.allowed_hosts.clone()));

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
fn files_route(store: Arc<AttachmentStore>, allowed_hosts: Vec<String>) -> Router {
    Router::new()
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
        .with_state(store)
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
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(async move { shutdown.cancelled_owned().await })
        .await;
    service_ct.cancel();
    result.context("the HTTP server exited with an error")
}

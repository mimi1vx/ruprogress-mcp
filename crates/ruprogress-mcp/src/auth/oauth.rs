//! `AuthMode::OAuth`: bearer-token extraction, RFC 7662 introspection with a
//! digest-keyed cache, and the axum middleware that issues the `401`/`503`
//! challenge before any MCP request reaches [`crate::server::RedmineMcp::scoped`].

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::request::Parts;
use http::{HeaderValue, StatusCode, header};
use redmine_client::model::introspection::Introspection;
use redmine_client::{Credential, RedmineClient};
use rmcp::ErrorData as McpError;
use rmcp::RoleServer;
use rmcp::service::RequestContext;
use secrecy::{ExposeSecret as _, SecretString};
use sha2::{Digest as _, Sha256};
use url::Url;

use crate::config::OAuthConfig;

/// Clock-skew allowance for `exp` comparisons (B6). Not configurable: a knob
/// nobody can set correctly, guarding a 5-second window.
const CLOCK_SKEW_SECS: i64 = 5;

/// How long an `active:false` (or expired) introspection result is cached
/// (O4).
const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(5);

/// Bounded cache capacity (O4). A new entry is simply not cached once full,
/// rather than evicting something a concurrent request might still need.
const CACHE_CAPACITY: usize = 1024;

/// A bearer token's maximum accepted length (B9): far above any real
/// Doorkeeper token, far below anything worth forwarding.
const MAX_TOKEN_LEN: usize = 4096;

/// The synthetic token [`TokenVerifier::probe`] introspects (D7). Never a
/// real credential — no real Doorkeeper token looks like this — so a
/// healthy endpoint always answers `{"active": false}`.
const PROBE_TOKEN: &str = "ruprogress-mcp-readiness-probe-token";

/// Outcome of [`TokenVerifier::probe`], consumed by `health::readyz` in
/// `oauth` mode (D7). Distinct from [`AuthError`]: there is no "invalid
/// token" case, because the probe's synthetic token introspecting as
/// inactive is itself the success case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeOutcome {
    /// Introspection answered and accepted this server's client credentials.
    Ok,
    /// Introspection rejected this server's own client credentials, or the
    /// route is unmounted.
    Misconfigured,
    /// Introspection could not be reached, or answered with a transport/5xx
    /// failure.
    Unreachable,
}

/// Validated per-request identity. Rides inside `http::request::Parts`,
/// which rmcp moves whole into the JSON-RPC request's extensions — so this
/// type's `Debug` must never print the token (B7).
#[derive(Clone)]
pub(crate) struct AuthContext {
    pub(crate) token: SecretString,
    pub(crate) subject: Option<String>,
    pub(crate) scopes: Arc<BTreeSet<String>>,
    pub(crate) expires_at: Option<i64>,
}

impl std::fmt::Debug for AuthContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthContext")
            .field("subject", &self.subject)
            .field("scopes", &self.scopes)
            .field("expires_at", &self.expires_at)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Why [`TokenVerifier::verify`] rejected a token, distinct from a bearer
/// *header* parsing failure (see [`BearerError`]).
#[derive(Debug, Clone, Copy)]
pub(crate) enum AuthError {
    /// The token introspected as inactive, or active with an `exp` inside
    /// the clock-skew window.
    InvalidToken(&'static str),
    /// Introspection could not be reached, or answered with a transport/5xx
    /// failure. Never the caller's fault (O7).
    Unavailable,
    /// Introspection rejected *this server's* client credentials, or the
    /// route is unmounted. Also never the caller's fault (O7).
    Misconfigured,
}

#[derive(Clone)]
enum CacheValue {
    Valid(AuthContext),
    Invalid,
}

struct CachedEntry {
    value: CacheValue,
    /// Monotonic cache expiry: `min(configured ttl, exp - now - skew)` for a
    /// positive entry, a fixed 5s for a negative one.
    expires_at: Instant,
}

/// Introspects and caches bearer tokens against Redmine's Doorkeeper
/// endpoint, scoped to the confidential introspection client (O3): this
/// reuses `RedmineClient`'s configured TLS/CA/timeout settings rather than a
/// second `reqwest::Client`.
pub(crate) struct TokenVerifier {
    client: RedmineClient,
    credential: Credential,
    ttl: Duration,
    cache: Mutex<HashMap<[u8; 32], CachedEntry>>,
}

impl std::fmt::Debug for TokenVerifier {
    // Manual: never print a cache key (a token digest) or entry.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        f.debug_struct("TokenVerifier")
            .field("ttl", &self.ttl)
            .field("cache_len", &len)
            .finish_non_exhaustive()
    }
}

impl TokenVerifier {
    pub(crate) fn new(client: RedmineClient, oauth: &OAuthConfig) -> Self {
        Self {
            client,
            credential: Credential::Basic {
                user: oauth.introspect_client_id.clone(),
                pass: oauth.introspect_client_secret.clone(),
            },
            ttl: oauth.token_cache_ttl,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn digest(token: &str) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        hasher.finalize().into()
    }

    /// `exp` is necessary but not sufficient (B6): a token within
    /// [`CLOCK_SKEW_SECS`] of its expiry is treated as already expired.
    fn is_expired(exp: Option<i64>) -> bool {
        let Some(exp) = exp else {
            return false;
        };
        let now = chrono::Utc::now().timestamp();
        exp <= now.saturating_add(CLOCK_SKEW_SECS)
    }

    /// Verify `token`: a cache hit, or a fresh RFC 7662 introspection on a
    /// miss. Never caches a transport/misconfiguration failure (O4).
    pub(crate) async fn verify(&self, token: SecretString) -> Result<AuthContext, AuthError> {
        let digest = Self::digest(token.expose_secret());

        if !self.ttl.is_zero()
            && let Some(cached) = self.cache_get(digest)
        {
            return cached;
        }

        let introspection = self.introspect(&token).await?;
        let result = Self::context_from(&introspection, token);
        if !self.ttl.is_zero() {
            self.cache_put(digest, &introspection, &result);
        }
        result
    }

    /// Remove a cached entry for `token`, if any. Called by `POST /revoke`
    /// (D5) after a successful upstream revocation, so a client that
    /// revokes its own token stops being accepted here immediately rather
    /// than for up to the cache TTL.
    pub(crate) fn purge(&self, token: &SecretString) {
        let digest = Self::digest(token.expose_secret());
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&digest);
    }

    /// Cache-bypassing readiness probe (D7), consumed by `health::readyz` in
    /// `oauth` mode. Introspects a synthetic token that no real Doorkeeper
    /// will ever recognise, so a healthy endpoint always answers `200
    /// {"active": false}`; the outcome that matters is not `active` but
    /// whether introspection was reachable and accepted our own client
    /// credentials at all.
    pub(crate) async fn probe(&self) -> ProbeOutcome {
        match self.introspect(&SecretString::from(PROBE_TOKEN)).await {
            Err(AuthError::Misconfigured) => ProbeOutcome::Misconfigured,
            Err(AuthError::Unavailable) => ProbeOutcome::Unreachable,
            // `introspect` never returns `InvalidToken`; `context_from` is
            // the only place that does, and this probe never calls it.
            Ok(_) | Err(AuthError::InvalidToken(_)) => ProbeOutcome::Ok,
        }
    }

    async fn introspect(&self, token: &SecretString) -> Result<Introspection, AuthError> {
        let scoped = self.client.as_user(&self.credential);
        match scoped.introspect_token(token).await {
            Ok(introspection) => Ok(introspection),
            Err(redmine_client::Error::Unauthorized | redmine_client::Error::Forbidden) => {
                tracing::error!(
                    "introspection rejected this server's own client credentials; check \
                     REDMINE_INTROSPECT_CLIENT_ID and REDMINE_INTROSPECT_CLIENT_SECRET"
                );
                Err(AuthError::Misconfigured)
            }
            Err(redmine_client::Error::NotFound) => {
                tracing::error!(
                    "introspection endpoint not found; Redmine's allow_token_introspection \
                     setting must be enabled for this OAuth application"
                );
                Err(AuthError::Misconfigured)
            }
            Err(error) => {
                tracing::warn!(%error, "introspection request failed");
                Err(AuthError::Unavailable)
            }
        }
    }

    fn context_from(
        introspection: &Introspection,
        token: SecretString,
    ) -> Result<AuthContext, AuthError> {
        if !introspection.active {
            return Err(AuthError::InvalidToken("token is not active"));
        }
        if Self::is_expired(introspection.exp) {
            return Err(AuthError::InvalidToken("token is expired"));
        }
        let scopes = introspection
            .scopes()
            .into_iter()
            .map(ToString::to_string)
            .collect();
        let subject = introspection
            .sub
            .clone()
            .or_else(|| introspection.username.clone());
        Ok(AuthContext {
            token,
            subject,
            scopes: Arc::new(scopes),
            expires_at: introspection.exp,
        })
    }

    fn cache_get(&self, digest: [u8; 32]) -> Option<Result<AuthContext, AuthError>> {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = cache.get(&digest)?;
        if entry.expires_at <= Instant::now() {
            cache.remove(&digest);
            return None;
        }
        match &entry.value {
            CacheValue::Valid(context) => {
                // Re-check `exp` on every hit (B6): the cache's own TTL is
                // already capped by it, but this is the belt to that
                // suspenders.
                if Self::is_expired(context.expires_at) {
                    cache.remove(&digest);
                    return None;
                }
                Some(Ok(context.clone()))
            }
            CacheValue::Invalid => Some(Err(AuthError::InvalidToken("token is not active"))),
        }
    }

    fn cache_put(
        &self,
        digest: [u8; 32],
        introspection: &Introspection,
        result: &Result<AuthContext, AuthError>,
    ) {
        let ttl = match result {
            Ok(_) => {
                let mut ttl = self.ttl;
                if let Some(exp) = introspection.exp {
                    let now = chrono::Utc::now().timestamp();
                    let remaining_secs = exp.saturating_sub(now).saturating_sub(CLOCK_SKEW_SECS);
                    // Already rejected by `context_from` when this would be
                    // non-positive, but a defensive `return` costs nothing.
                    if remaining_secs <= 0 {
                        return;
                    }
                    let remaining =
                        Duration::from_secs(u64::try_from(remaining_secs).unwrap_or(u64::MAX));
                    ttl = ttl.min(remaining);
                }
                ttl
            }
            Err(AuthError::InvalidToken(_)) => NEGATIVE_CACHE_TTL,
            // Transport/misconfiguration failures are never cached (O4): the
            // next request should get a fresh chance to see the outage end.
            Err(AuthError::Unavailable | AuthError::Misconfigured) => return,
        };
        if ttl.is_zero() {
            return;
        }

        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        cache.retain(|_, entry| entry.expires_at > now);
        if cache.len() >= CACHE_CAPACITY && !cache.contains_key(&digest) {
            // Degrade gracefully: skip caching this one rather than evict a
            // still-live entry another concurrent request may need.
            return;
        }
        let value = match result {
            Ok(context) => CacheValue::Valid(context.clone()),
            Err(_) => CacheValue::Invalid,
        };
        cache.insert(
            digest,
            CachedEntry {
                value,
                expires_at: now.checked_add(ttl).unwrap_or(now),
            },
        );
    }
}

/// Reads the `AuthContext` the bearer-auth middleware inserted into
/// `Parts.extensions` — mirroring how `auth::per_user::credential` reads
/// inbound headers from the same `Parts` — and fails closed (an internal
/// error, never a fallback) if it is absent, which can only happen if a
/// future refactor mounts the MCP route without [`layer`]. Shared by
/// [`scoped`] (O2) and `server.rs`'s hand-written `list_tools`/`call_tool`,
/// which need the token's scopes rather than a `Scoped` credential.
///
/// # Errors
///
/// Returns an [`McpError`] if the request did not arrive over HTTP, or if
/// the bearer-auth middleware did not run for it.
pub(crate) fn auth_context(ctx: &RequestContext<RoleServer>) -> Result<&AuthContext, McpError> {
    let parts = ctx.extensions.get::<Parts>().ok_or_else(|| {
        McpError::internal_error(
            "oauth auth mode requires the HTTP transport (no request headers available)",
            None,
        )
    })?;
    parts.extensions.get::<AuthContext>().ok_or_else(|| {
        McpError::internal_error(
            "oauth auth mode requires the bearer-auth middleware, which did not run for this \
             request",
            None,
        )
    })
}

/// The `AuthMode::OAuth` arm of `RedmineMcp::scoped` (O2).
///
/// # Errors
///
/// See [`auth_context`].
pub(crate) fn scoped<'c>(
    client: &'c RedmineClient,
    ctx: &RequestContext<RoleServer>,
) -> Result<redmine_client::Scoped<'c>, McpError> {
    let auth = auth_context(ctx)?;
    tracing::debug!(subject = ?auth.subject, "oauth request");
    Ok(client.as_user_owned(Credential::Bearer(auth.token.clone())))
}

/// The pre-built `WWW-Authenticate` challenge (B10): a pure function of
/// config, computed once at startup so every `401` response carries a
/// byte-identical value.
#[derive(Debug, Clone)]
pub(crate) struct Challenge {
    /// The full challenge with no `error` parameter, e.g.
    /// `Bearer resource_metadata="https://host/.well-known/oauth-protected-resource/mcp"`.
    base: String,
}

impl Challenge {
    pub(crate) fn build(base_url: &Url, mcp_path: &str) -> Self {
        let raw = base_url.as_str();
        let trimmed = raw.strip_suffix('/').unwrap_or(raw);
        Self {
            base: format!(
                r#"Bearer resource_metadata="{trimmed}/.well-known/oauth-protected-resource{mcp_path}""#
            ),
        }
    }

    fn header_value(&self, error: Option<&str>) -> HeaderValue {
        let value = match error {
            Some(error) => format!(r#"{}, error="{error}""#, self.base),
            None => self.base.clone(),
        };
        HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static("Bearer"))
    }
}

/// Why [`extract_bearer`] rejected the `Authorization` header, distinct from
/// [`AuthError`] (which only ever applies once a token has been extracted).
/// `pub(crate)`: also matched by `auth::proxy`'s middleware, which shares
/// this extraction step (F10, F11).
#[derive(Debug)]
pub(crate) enum BearerError {
    /// No `Authorization` header at all: the ordinary "client has no token
    /// yet" case, so the challenge carries no `error` parameter.
    Missing,
    /// A header was present but did not parse as a well-formed bearer
    /// token (B9): duplicated, wrong scheme, malformed, oversized, or
    /// non-visible-ASCII.
    Malformed(&'static str),
}

/// Extracts and validates the inbound bearer token per B9. Never falls back
/// to a different scheme or a second header value: header smuggling through
/// a misconfigured proxy is the realistic attack this guards against.
///
/// `pub(crate)`: shared with `auth::proxy`'s middleware (F10, F11) — both
/// modes need the identical extraction/validation step, only what happens
/// with the extracted token differs.
pub(crate) fn extract_bearer(headers: &http::HeaderMap) -> Result<SecretString, BearerError> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let Some(value) = values.next() else {
        return Err(BearerError::Missing);
    };
    if values.next().is_some() {
        return Err(BearerError::Malformed(
            "Authorization header must be sent exactly once",
        ));
    }
    let text = value.to_str().map_err(|_| {
        BearerError::Malformed(
            "Authorization header contains invalid (non-visible-ASCII) characters",
        )
    })?;
    let Some((scheme, token)) = text.split_once(' ') else {
        return Err(BearerError::Malformed(
            "Authorization header must be \"Bearer <token>\"",
        ));
    };
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(BearerError::Malformed(
            "Authorization header must use the Bearer scheme",
        ));
    }
    if token.trim() != token {
        return Err(BearerError::Malformed(
            "Authorization header must have exactly one space between the scheme and the token",
        ));
    }
    if token.is_empty() {
        return Err(BearerError::Malformed(
            "Authorization header token must not be empty",
        ));
    }
    if token.len() > MAX_TOKEN_LEN {
        return Err(BearerError::Malformed(
            "Authorization header token exceeds the maximum accepted length",
        ));
    }
    if !token.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(BearerError::Malformed(
            "Authorization header token contains invalid (non-visible-ASCII) characters",
        ));
    }
    Ok(SecretString::from(token.to_string()))
}

/// Shared state for [`require_bearer`]: cheap to clone (two `Arc` bumps),
/// which is what `axum::middleware::from_fn_with_state` requires.
pub(crate) type AuthState = (Arc<TokenVerifier>, Arc<Challenge>);

/// The `oauth` mode's authentication middleware (O1): every request must
/// carry a valid bearer token, including `initialize`. Never mounted outside
/// this auth mode, and only ever on the MCP route (O8) — see the `SECURITY:`
/// comment in `transport::http::router`.
pub(crate) async fn require_bearer(
    State((verifier, challenge)): State<AuthState>,
    mut req: Request,
    next: Next,
) -> Response {
    let token = match extract_bearer(req.headers()) {
        Ok(token) => token,
        Err(BearerError::Missing) => return challenge_response(&challenge, None),
        Err(BearerError::Malformed(reason)) => {
            tracing::warn!(reason, "rejected a malformed Authorization header");
            return challenge_response(&challenge, Some("invalid_request"));
        }
    };

    match verifier.verify(token).await {
        Ok(context) => {
            req.extensions_mut().insert(context);
            next.run(req).await
        }
        Err(AuthError::InvalidToken(reason)) => {
            tracing::warn!(reason, "rejected an invalid bearer token");
            challenge_response(&challenge, Some("invalid_token"))
        }
        Err(AuthError::Unavailable) => {
            tracing::warn!("introspection is unavailable; rejecting with 503, not 401");
            unavailable_response()
        }
        Err(AuthError::Misconfigured) => {
            // The specific misconfiguration was already logged at ERROR by
            // `TokenVerifier::introspect`.
            unavailable_response()
        }
    }
}

/// `pub(crate)`: also used by `auth::proxy`'s `oauth-proxy` middleware
/// (F10, F11), which shares this exact challenge shape.
pub(crate) fn challenge_response(challenge: &Challenge, error: Option<&str>) -> Response {
    let mut response = (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, challenge.header_value(error));
    response
}

/// `503` + `Retry-After` (O7): introspection being broken is never the
/// caller's fault, so this must never look like an invalid-token `401`.
/// `pub(crate)`: also used by `auth::proxy`'s middleware (F10), which hits
/// the same introspection unavailability case through the same
/// `TokenVerifier`.
pub(crate) fn unavailable_response() -> Response {
    let mut response = (StatusCode::SERVICE_UNAVAILABLE, "service unavailable").into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    response
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> http::HeaderMap {
        let mut builder = http::Request::builder();
        for (name, value) in pairs {
            builder = builder.header(*name, *value);
        }
        let (parts, ()) = builder.body(()).expect("request should build").into_parts();
        parts.headers
    }

    fn token_of(result: &Result<SecretString, BearerError>) -> &str {
        result.as_ref().expect("expected Ok").expose_secret()
    }

    #[test]
    fn missing_header_is_missing() {
        let h = headers(&[]);
        assert!(matches!(extract_bearer(&h), Err(BearerError::Missing)));
    }

    #[test]
    fn duplicated_header_is_malformed() {
        let h = headers(&[
            ("authorization", "Bearer one"),
            ("authorization", "Bearer two"),
        ]);
        assert!(matches!(extract_bearer(&h), Err(BearerError::Malformed(_))));
    }

    #[test]
    fn non_bearer_scheme_is_malformed() {
        let h = headers(&[("authorization", "Basic dXNlcjpwYXNz")]);
        assert!(matches!(extract_bearer(&h), Err(BearerError::Malformed(_))));
    }

    #[test]
    fn empty_token_is_malformed() {
        let h = headers(&[("authorization", "Bearer ")]);
        assert!(matches!(extract_bearer(&h), Err(BearerError::Malformed(_))));
    }

    #[test]
    fn oversized_token_is_malformed() {
        let value = format!("Bearer {}", "a".repeat(MAX_TOKEN_LEN + 1));
        let h = headers(&[("authorization", &value)]);
        assert!(matches!(extract_bearer(&h), Err(BearerError::Malformed(_))));
    }

    #[test]
    fn extra_whitespace_is_malformed() {
        let h = headers(&[("authorization", "Bearer  leading-double-space")]);
        assert!(matches!(extract_bearer(&h), Err(BearerError::Malformed(_))));
    }

    #[test]
    fn non_ascii_token_is_malformed() {
        let mut map = http::HeaderMap::new();
        map.insert(
            header::AUTHORIZATION,
            HeaderValue::from_bytes(b"Bearer abc\xC3\xA9def").expect("opaque bytes construct"),
        );
        assert!(matches!(
            extract_bearer(&map),
            Err(BearerError::Malformed(_))
        ));
    }

    #[test]
    fn happy_path_is_case_insensitive_on_scheme() {
        let h = headers(&[("authorization", "bearer the-token")]);
        let result = extract_bearer(&h);
        assert_eq!(token_of(&result), "the-token");
    }

    #[test]
    fn is_expired_treats_none_as_not_expired() {
        assert!(!TokenVerifier::is_expired(None));
    }

    #[test]
    fn is_expired_respects_the_clock_skew_window() {
        let now = chrono::Utc::now().timestamp();
        assert!(TokenVerifier::is_expired(Some(now + CLOCK_SKEW_SECS - 1)));
        assert!(!TokenVerifier::is_expired(Some(now + CLOCK_SKEW_SECS + 60)));
    }

    #[test]
    fn challenge_has_no_error_param_by_default() {
        let base = "https://mcp.example.com".parse().expect("valid url");
        let challenge = Challenge::build(&base, "/mcp");
        let value = challenge.header_value(None);
        assert_eq!(
            value.to_str().unwrap(),
            r#"Bearer resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource/mcp""#
        );
    }

    #[test]
    fn challenge_strips_a_trailing_slash_from_the_base() {
        let base = "https://mcp.example.com/".parse().expect("valid url");
        let challenge = Challenge::build(&base, "/mcp");
        let value = challenge.header_value(None);
        assert!(!value.to_str().unwrap().contains("com//"));
    }

    #[test]
    fn challenge_appends_the_error_parameter() {
        let base = "https://mcp.example.com".parse().expect("valid url");
        let challenge = Challenge::build(&base, "/mcp");
        let value = challenge.header_value(Some("invalid_token"));
        assert!(
            value
                .to_str()
                .unwrap()
                .ends_with(r#", error="invalid_token""#)
        );
    }

    #[test]
    fn debug_of_auth_context_never_contains_the_token() {
        const TOKEN: &str = "super-secret-access-token";
        let context = AuthContext {
            token: SecretString::from(TOKEN),
            subject: Some("alice".to_string()),
            scopes: Arc::new(BTreeSet::new()),
            expires_at: None,
        };
        let rendered = format!("{context:?}");
        assert!(!rendered.contains(TOKEN));
        assert!(rendered.contains("alice"));
    }

    #[test]
    fn debug_of_token_verifier_never_contains_a_key() {
        let client = redmine_client::RedmineClientBuilder::new(
            "https://example.com".parse().expect("valid url"),
        )
        .build()
        .expect("client should build");
        let oauth = OAuthConfig {
            base_url: "https://example.com".parse().expect("valid url"),
            introspect_client_id: "client-id".to_string(),
            introspect_client_secret: SecretString::from("client-secret"),
            token_cache_ttl: Duration::from_mins(1),
            discovery_as: crate::config::DiscoveryAs::Redmine,
            scopes: Vec::new(),
            scope_enforcement: true,
        };
        let verifier = TokenVerifier::new(client, &oauth);
        let rendered = format!("{verifier:?}");
        assert!(!rendered.contains("client-secret"));
        assert!(rendered.contains("cache_len"));
    }
}

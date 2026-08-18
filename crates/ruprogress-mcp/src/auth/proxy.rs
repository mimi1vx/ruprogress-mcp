//! `oauth-proxy`'s MCP-route authentication middleware (P9, F10, F11): a
//! token-store lookup swapped in ahead of `auth::oauth::TokenVerifier`.
//! `RedmineMcp::scoped`'s `AuthMode::OAuthProxy` arm shares `oauth` mode's
//! arm outright (`server.rs`) — both modes end up with the identical
//! `AuthContext` shape, so there is nothing left to add there.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use secrecy::ExposeSecret as _;

use crate::auth::oauth::{self, AuthError, BearerError, Challenge, TokenVerifier};
use crate::oauth::proxy::store::{TokenStore, UpstreamStore};

/// Prefix every minted proxy access token carries (P2, F9). Anything else
/// presented here — including a raw upstream Redmine token — is
/// `invalid_token` without ever reaching a store lookup: P9 forbids falling
/// back to accepting an upstream token directly, which would silently
/// downgrade this mode into `oauth`.
const ACCESS_TOKEN_PREFIX: &str = "rup_at_";

/// Shared state for [`require_proxy_bearer`]: cheap to clone (four `Arc`
/// bumps), which is what `axum::middleware::from_fn_with_state` requires.
#[derive(Clone)]
pub(crate) struct ProxyAuthState {
    pub(crate) tokens: Arc<TokenStore>,
    pub(crate) upstream_tokens: Arc<UpstreamStore>,
    pub(crate) verifier: Arc<TokenVerifier>,
    pub(crate) challenge: Arc<Challenge>,
}

/// The `oauth-proxy` mode's authentication middleware (F10). Resolves a
/// presented `rup_at_...` token to the upstream Redmine access token it
/// stands for, then verifies *that* exactly as `oauth` mode verifies a
/// token it received directly — so 6b3's scope enforcement, in-band
/// `INSUFFICIENT_SCOPE` denial, and identity audit apply here with no new
/// policy code (P9). Never mounted outside this auth mode, and only ever on
/// the MCP route — see the `SECURITY:` comment in `transport::http::router`.
pub(crate) async fn require_proxy_bearer(
    State(state): State<ProxyAuthState>,
    mut req: Request,
    next: Next,
) -> Response {
    let token = match oauth::extract_bearer(req.headers()) {
        Ok(token) => token,
        Err(BearerError::Missing) => return oauth::challenge_response(&state.challenge, None),
        Err(BearerError::Malformed(reason)) => {
            tracing::warn!(reason, "rejected a malformed Authorization header");
            return oauth::challenge_response(&state.challenge, Some("invalid_request"));
        }
    };

    if !token.expose_secret().starts_with(ACCESS_TOKEN_PREFIX) {
        tracing::warn!("rejected a bearer token without the oauth-proxy access-token prefix");
        return oauth::challenge_response(&state.challenge, Some("invalid_token"));
    }

    let Some(entry) = state.tokens.resolve(token.expose_secret()) else {
        return oauth::challenge_response(&state.challenge, Some("invalid_token"));
    };
    let Some(upstream_access) = state.upstream_tokens.access_token(&entry.upstream_id) else {
        return oauth::challenge_response(&state.challenge, Some("invalid_token"));
    };

    match state.verifier.verify(upstream_access).await {
        Ok(context) => {
            req.extensions_mut().insert(context);
            next.run(req).await
        }
        Err(AuthError::InvalidToken(reason)) => {
            tracing::warn!(
                reason,
                "the upstream token behind this proxy token is no longer valid"
            );
            oauth::challenge_response(&state.challenge, Some("invalid_token"))
        }
        Err(AuthError::Unavailable | AuthError::Misconfigured) => {
            tracing::warn!("introspection is unavailable; rejecting with 503, not 401");
            oauth::unavailable_response()
        }
    }
}

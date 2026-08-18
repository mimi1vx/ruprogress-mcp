//! Liveness and readiness endpoints for the HTTP transport.
//!
//! These are unauthenticated, so the body carries readiness facts only —
//! `status`, `redmine`, `checked_at` — and nothing else. No Redmine URL, no
//! bind address, no version, no auth mode, no plugin flags: an unauthenticated
//! prober learns whether we are ready to serve, and nothing about how we are
//! deployed. `get_mcp_server_info` (authenticated) and `--print-config`
//! (local) remain the places to look for configuration.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tokio::sync::Mutex;

use crate::auth::oauth::ProbeOutcome as IntrospectionOutcome;
use crate::server::RedmineMcp;

/// How long a single Redmine probe is allowed to take before `/readyz` calls
/// it down. Deliberately well under any sensible probe interval.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub(crate) struct HealthState {
    server: RedmineMcp,
    ttl: Duration,
    cache: Arc<Mutex<Option<CachedProbe>>>,
}

#[derive(Clone, Copy)]
struct CachedProbe {
    /// For TTL arithmetic — monotonic, unlike the wall clock.
    at: Instant,
    /// For the `checked_at` field, which must report when the probe actually
    /// ran and not when the cache hit was served.
    checked_at: chrono::DateTime<chrono::Utc>,
    result: ProbeResult,
}

/// What [`probe`] found, one variant per auth mode that owns a
/// server-side credential worth probing (O9): `legacy`'s own Redmine
/// credential, or `oauth`'s introspection client. `legacy-per-user` has
/// neither, so it stays `Redmine(Readiness::NotProbed)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ProbeResult {
    Redmine(Readiness),
    Introspection(IntrospectionOutcome),
}

impl ProbeResult {
    fn is_ready(self) -> bool {
        match self {
            Self::Redmine(r) => r != Readiness::Down,
            Self::Introspection(i) => i == IntrospectionOutcome::Ok,
        }
    }

    fn status_code(self) -> StatusCode {
        match self {
            Self::Redmine(r) => r.status_code(),
            Self::Introspection(IntrospectionOutcome::Ok) => StatusCode::OK,
            Self::Introspection(
                IntrospectionOutcome::Misconfigured | IntrospectionOutcome::Unreachable,
            ) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// The `redmine` field's value: kept for both variants so the body's
    /// shape (three keys, plus `checks` in oauth mode only) does not depend
    /// on which probe ran.
    fn redmine_label(self) -> &'static str {
        match self {
            Self::Redmine(r) => r.label(),
            Self::Introspection(i) => introspection_label(i),
        }
    }
}

fn introspection_label(outcome: IntrospectionOutcome) -> &'static str {
    match outcome {
        IntrospectionOutcome::Ok => "ok",
        IntrospectionOutcome::Misconfigured => "misconfigured",
        IntrospectionOutcome::Unreachable => "unreachable",
    }
}

impl std::fmt::Debug for HealthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthState")
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl HealthState {
    pub(crate) fn new(server: RedmineMcp, ttl: Duration) -> Self {
        Self {
            server,
            ttl,
            cache: Arc::new(Mutex::new(None)),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Readiness {
    Up,
    Down,
    /// No server-owned credential exists to probe with (`legacy-per-user`,
    /// `oauth`). Reporting "down" here would be a lie that takes the instance
    /// out of rotation permanently.
    NotProbed,
}

impl Readiness {
    fn label(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::NotProbed => "not_probed",
        }
    }

    fn status_code(self) -> StatusCode {
        match self {
            Self::Up | Self::NotProbed => StatusCode::OK,
            Self::Down => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

fn no_store(status: StatusCode, body: serde_json::Value) -> Response {
    let mut response = (status, axum::Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Process liveness. Never checks Redmine — a dependency blip must not be
/// able to trigger a restart storm, which is the entire reason this endpoint
/// is separate from `/readyz`.
pub(crate) async fn livez() -> Response {
    no_store(StatusCode::OK, json!({ "status": "alive" }))
}

/// Readiness, backed by a TTL-cached probe: Redmine's `current_user` in
/// `legacy` mode, Doorkeeper introspection in `oauth` mode (O9), neither in
/// `legacy-per-user`.
pub(crate) async fn readyz(State(state): State<HealthState>) -> Response {
    let probe = probe_cached(&state).await;
    let status = if probe.result.is_ready() {
        "ready"
    } else {
        "not_ready"
    };
    let body = match probe.result {
        ProbeResult::Redmine(_) => json!({
            "status": status,
            "redmine": probe.result.redmine_label(),
            "checked_at": probe.checked_at.to_rfc3339(),
        }),
        // `checks.introspection` (D7) is additive: every other field keeps
        // the same shape legacy mode already carries.
        ProbeResult::Introspection(outcome) => json!({
            "status": status,
            "redmine": probe.result.redmine_label(),
            "checks": { "introspection": introspection_label(outcome) },
            "checked_at": probe.checked_at.to_rfc3339(),
        }),
    };
    no_store(probe.result.status_code(), body)
}

async fn probe_cached(state: &HealthState) -> CachedProbe {
    if state.ttl.is_zero() {
        // Caching is off, so taking the lock would only serialize probes —
        // turning N concurrent requests into N sequential upstream calls, each
        // able to burn the full `PROBE_TIMEOUT`.
        return run_probe(state).await;
    }
    // Probing *inside* the lock is the point: concurrent probes collapse into
    // a single upstream request instead of amplifying a health check into a
    // stampede against Redmine.
    let mut cache = state.cache.lock().await;
    if let Some(cached) = *cache
        && cached.at.elapsed() < state.ttl
    {
        return cached;
    }
    let fresh = run_probe(state).await;
    *cache = Some(fresh);
    fresh
}

async fn run_probe(state: &HealthState) -> CachedProbe {
    let result = probe(state).await;
    CachedProbe {
        at: Instant::now(),
        checked_at: chrono::Utc::now(),
        result,
    }
}

async fn probe(state: &HealthState) -> ProbeResult {
    if let Some(verifier) = state.server.verifier() {
        return ProbeResult::Introspection(probe_introspection(&verifier).await);
    }
    ProbeResult::Redmine(probe_redmine(state).await)
}

/// Routed to by any auth mode that owns a [`crate::auth::oauth::TokenVerifier`]
/// (`server.verifier()` is `Some`), rather than by matching `AuthMode`
/// directly — so a future mode sharing the same verifier is probed the same
/// way with no new arm to remember.
async fn probe_introspection(verifier: &crate::auth::oauth::TokenVerifier) -> IntrospectionOutcome {
    if let Ok(outcome) = tokio::time::timeout(PROBE_TIMEOUT, verifier.probe()).await {
        outcome
    } else {
        tracing::debug!("readiness probe against introspection timed out");
        IntrospectionOutcome::Unreachable
    }
}

async fn probe_redmine(state: &HealthState) -> Readiness {
    let Some(scoped) = state.server.server_scoped() else {
        return Readiness::NotProbed;
    };
    let Ok(scoped) = scoped else {
        return Readiness::Down;
    };
    match tokio::time::timeout(PROBE_TIMEOUT, scoped.current_user()).await {
        Ok(Ok(_)) => Readiness::Up,
        Ok(Err(error)) => {
            tracing::debug!(%error, "readiness probe against Redmine failed");
            Readiness::Down
        }
        Err(_) => {
            tracing::debug!("readiness probe against Redmine timed out");
            Readiness::Down
        }
    }
}

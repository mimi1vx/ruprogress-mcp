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
    readiness: Readiness,
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

/// Readiness, backed by a TTL-cached Redmine probe.
pub(crate) async fn readyz(State(state): State<HealthState>) -> Response {
    let probe = probe_cached(&state).await;
    let status = if probe.readiness == Readiness::Down {
        "not_ready"
    } else {
        "ready"
    };
    no_store(
        probe.readiness.status_code(),
        json!({
            "status": status,
            "redmine": probe.readiness.label(),
            "checked_at": probe.checked_at.to_rfc3339(),
        }),
    )
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
    let readiness = probe(state).await;
    CachedProbe {
        at: Instant::now(),
        checked_at: chrono::Utc::now(),
        readiness,
    }
}

async fn probe(state: &HealthState) -> Readiness {
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

//! Shared e2e harness: a real `RedmineMcp` server wired to a `wiremock`
//! Redmine, reachable either over an in-process `tokio::io::duplex` pair
//! (confirmed working without any extra `rmcp` feature — see ADR 0005) or over
//! a real loopback TCP socket. Used by every test file in this crate.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use std::collections::BTreeMap;

use redmine_client::RedmineClientBuilder;
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt as _};
use ruprogress_mcp::config::{AuthMode, Config, TransportKind};
use ruprogress_mcp::server::RedmineMcp;
use tokio_util::sync::CancellationToken;

pub(crate) struct Harness {
    pub(crate) redmine: wiremock::MockServer,
    pub(crate) client: RunningService<RoleClient, ()>,
}

/// Build a `RedmineMcp` against `redmine`, using `env` on top of the default
/// valid legacy config (`REDMINE_URL` = the mock server, `REDMINE_API_KEY` = a
/// test key).
fn build_server(
    redmine: &wiremock::MockServer,
    env: &[(&str, &str)],
    kind: TransportKind,
) -> (RedmineMcp, Config) {
    let mut vars: BTreeMap<String, String> = env
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    vars.entry("REDMINE_URL".to_string())
        .or_insert_with(|| redmine.uri());
    vars.entry("REDMINE_API_KEY".to_string())
        .or_insert_with(|| "test-api-key".to_string());

    let config = Config::from_map(&vars, kind).expect("test config should be valid");

    let mut builder = RedmineClientBuilder::new(config.redmine.url.clone());
    if let AuthMode::Legacy { credential } = &config.auth {
        builder = builder.credential(credential.clone());
    }
    let redmine_client = builder.build().expect("redmine client should build");

    (RedmineMcp::new(redmine_client, config.clone()), config)
}

/// Start a mock Redmine and an in-process `RedmineMcp` server pointed at it,
/// connected via an in-memory duplex pipe (no real stdio/sockets involved).
pub(crate) async fn harness(env: &[(&str, &str)]) -> Harness {
    let redmine = wiremock::MockServer::start().await;
    let (server, _) = build_server(&redmine, env, TransportKind::Stdio);

    let (server_transport, client_transport) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let running = server
            .serve(server_transport)
            .await
            .expect("server should start serving over the duplex transport");
        let _ = running.waiting().await;
    });

    let client =
        ().serve(client_transport)
            .await
            .expect("client should connect over the duplex transport");

    Harness { redmine, client }
}

pub(crate) struct HttpHarness {
    pub(crate) redmine: wiremock::MockServer,
    /// `http://127.0.0.1:<ephemeral>` — no trailing slash.
    pub(crate) base_url: String,
    /// Stops accepting and starts axum's graceful drain. Kept separate from
    /// the service token for the same reason production does: cancelling
    /// rmcp's token aborts in-flight tool calls.
    pub(crate) shutdown: CancellationToken,
    service_ct: CancellationToken,
}

impl HttpHarness {
    pub(crate) fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base_url)
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

impl Drop for HttpHarness {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.service_ct.cancel();
    }
}

/// Start a mock Redmine and serve the real HTTP router on an ephemeral
/// loopback port. Goes through `transport::http::router`, so every tower layer
/// and rmcp edge check under test is the one production uses.
pub(crate) async fn http_harness(env: &[(&str, &str)]) -> HttpHarness {
    let redmine = wiremock::MockServer::start().await;
    let (server, config) = build_server(&redmine, env, TransportKind::Http);
    let http = config
        .transport
        .as_http()
        .expect("http transport requested")
        .clone();

    let service_ct = CancellationToken::new();
    let router = ruprogress_mcp::transport::http::router(server, &http, service_ct.clone());

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind an ephemeral loopback port");
    let addr = listener.local_addr().expect("read the bound address");

    let shutdown = CancellationToken::new();
    let signal = shutdown.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { signal.cancelled_owned().await })
            .await;
    });

    HttpHarness {
        redmine,
        base_url: format!("http://127.0.0.1:{}", addr.port()),
        shutdown,
        service_ct,
    }
}

/// Mock `GET /my/account.json` with a fixture user, `times` responses expected.
pub(crate) async fn mock_current_user(redmine: &wiremock::MockServer, times: Option<u64>) {
    use wiremock::matchers::{method, path};
    let mut mock = wiremock::Mock::given(method("GET"))
        .and(path("/my/account.json"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user": {
                    "id": 5,
                    "login": "alice",
                    "firstname": "Alice",
                    "lastname": "Example",
                    "mail": "alice@example.com",
                    "created_on": "2024-01-01T00:00:00Z",
                    "last_login_on": "2026-08-01T00:00:00Z",
                }
            })),
        );
    if let Some(times) = times {
        mock = mock.expect(times);
    }
    mock.mount(redmine).await;
}

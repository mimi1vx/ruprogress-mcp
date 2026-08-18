//! Shared e2e harness: a real `RedmineMcp` server wired to a `wiremock`
//! Redmine, reachable either over an in-process `tokio::io::duplex` pair
//! (confirmed working without any extra `rmcp` feature — see ADR 0005) or over
//! a real loopback TCP socket. Used by every test file in this crate.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use redmine_client::RedmineClientBuilder;
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt as _};
use ruprogress_mcp::attachments::AttachmentStore;
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
    // A unique dir per server, not the shared per-machine default: tests run
    // concurrently and must not trip over each other's attachment files.
    vars.entry("ATTACHMENTS_DIR".to_string())
        .or_insert_with(|| {
            std::env::temp_dir()
                .join(format!(
                    "ruprogress-mcp-test-attachments-{}-{}",
                    std::process::id(),
                    uuid::Uuid::new_v4()
                ))
                .to_string_lossy()
                .into_owned()
        });

    let config = Config::from_map(&vars, kind).expect("test config should be valid");

    let mut builder = RedmineClientBuilder::new(config.redmine.url.clone());
    if let AuthMode::Legacy { credential } = &config.auth {
        builder = builder.credential(credential.clone());
    }
    let redmine_client = builder.build().expect("redmine client should build");
    let attachments = Arc::new(
        AttachmentStore::init(&config.attachments).expect("attachment store should initialize"),
    );

    (
        RedmineMcp::new(redmine_client, config.clone(), attachments),
        config,
    )
}

/// Start a mock Redmine and an in-process `RedmineMcp` server pointed at it,
/// connected via an in-memory duplex pipe (no real stdio/sockets involved).
pub(crate) async fn harness(env: &[(&str, &str)]) -> Harness {
    let redmine = wiremock::MockServer::start().await;
    let (server, _) = build_server(&redmine, env, TransportKind::Stdio);
    serve_over_duplex(redmine, server).await
}

/// Like [`harness`], but registers `route` on the server before it starts
/// serving — for tests that need a handler `build_server`'s fixed tool
/// routers don't provide (`tests/panic_containment.rs`'s panicking tool).
pub(crate) async fn harness_with_route(
    env: &[(&str, &str)],
    route: rmcp::handler::server::router::tool::ToolRoute<RedmineMcp>,
) -> Harness {
    let redmine = wiremock::MockServer::start().await;
    let (mut server, _) = build_server(&redmine, env, TransportKind::Stdio);
    server.add_test_route(route);
    serve_over_duplex(redmine, server).await
}

async fn serve_over_duplex(redmine: wiremock::MockServer, server: RedmineMcp) -> Harness {
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
    /// The same store handle `/files/{uuid}` serves from, for tests that
    /// need to populate an entry directly rather than through a tool.
    pub(crate) attachments: Arc<AttachmentStore>,
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

    let attachments = server.attachments();
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
        attachments,
        shutdown,
        service_ct,
    }
}

/// Assert `result.structured_content` is present, is a JSON **object**,
/// and validates against `schema` (a tool's declared `outputSchema`).
///
/// This is a minimal structural check — object/array/scalar `type`,
/// `properties`, `required`, array `items` — not a general JSON Schema
/// validator. Sufficient for our own hand-written, deliberately flat output
/// schemas; adopt a real validator crate if a future schema needs
/// `anyOf`/`oneOf`/`$ref`.
pub(crate) fn assert_structured_content_matches_schema(
    result: &rmcp::model::CallToolResult,
    schema: &serde_json::Map<String, serde_json::Value>,
) {
    let structured = result
        .structured_content
        .as_ref()
        .expect("tool result must carry structured_content");
    assert!(
        structured.is_object(),
        "structuredContent must be a JSON object, got {structured}"
    );
    assert_schema(structured, &serde_json::Value::Object(schema.clone()), "$");
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn assert_schema(value: &serde_json::Value, schema: &serde_json::Value, path: &str) {
    let Some(schema_obj) = schema.as_object() else {
        return;
    };

    if let Some(ty) = schema_obj.get("type") {
        let allowed: Vec<&str> = match ty {
            serde_json::Value::String(s) => vec![s.as_str()],
            serde_json::Value::Array(arr) => arr.iter().filter_map(|v| v.as_str()).collect(),
            _ => vec![],
        };
        let actual = json_type_name(value);
        // A JSON integer also satisfies a schema `"type": "number"`.
        let matches =
            allowed.contains(&actual) || (actual == "integer" && allowed.contains(&"number"));
        assert!(
            matches,
            "{path}: expected type in {allowed:?}, got {actual} ({value})"
        );
    }

    if let Some(props) = schema_obj.get("properties").and_then(|p| p.as_object())
        && let Some(obj) = value.as_object()
    {
        for (key, sub_schema) in props {
            if let Some(sub_value) = obj.get(key) {
                assert_schema(sub_value, sub_schema, &format!("{path}.{key}"));
            }
        }
    }

    if let Some(required) = schema_obj.get("required").and_then(|r| r.as_array())
        && let Some(obj) = value.as_object()
    {
        for req in required {
            if let Some(key) = req.as_str() {
                assert!(
                    obj.contains_key(key),
                    "{path}: missing required field {key:?}"
                );
            }
        }
    }

    if let Some(items_schema) = schema_obj.get("items")
        && let Some(arr) = value.as_array()
    {
        for (i, item) in arr.iter().enumerate() {
            assert_schema(item, items_schema, &format!("{path}[{i}]"));
        }
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

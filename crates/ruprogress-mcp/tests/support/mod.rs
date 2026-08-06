//! Shared e2e harness: a real `RedmineMcp` server wired to a `wiremock`
//! Redmine, talked to over an in-process `tokio::io::duplex` pair (confirmed
//! working without any extra `rmcp` feature — see ADR 0005). Used by every
//! test file in this crate.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use std::collections::BTreeMap;

use redmine_client::RedmineClientBuilder;
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt as _};
use ruprogress_mcp::config::{AuthMode, Config, TransportConfig};
use ruprogress_mcp::server::RedmineMcp;

pub(crate) struct Harness {
    pub(crate) redmine: wiremock::MockServer,
    pub(crate) client: RunningService<RoleClient, ()>,
}

/// Start a mock Redmine and an in-process `RedmineMcp` server pointed at it,
/// connected via an in-memory duplex pipe (no real stdio/sockets involved).
/// `env` overrides/extends the default valid legacy config
/// (`REDMINE_URL` = the mock server, `REDMINE_API_KEY` = a test key).
pub(crate) async fn harness(env: &[(&str, &str)]) -> Harness {
    let redmine = wiremock::MockServer::start().await;

    let mut vars: BTreeMap<String, String> = env
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    vars.entry("REDMINE_URL".to_string())
        .or_insert_with(|| redmine.uri());
    vars.entry("REDMINE_API_KEY".to_string())
        .or_insert_with(|| "test-api-key".to_string());

    let config =
        Config::from_map(&vars, TransportConfig::Stdio).expect("test config should be valid");

    let mut builder = RedmineClientBuilder::new(config.redmine.url.clone());
    if let AuthMode::Legacy { credential } = &config.auth {
        builder = builder.credential(credential.clone());
    }
    let redmine_client = builder.build().expect("redmine client should build");

    let server = RedmineMcp::new(redmine_client, config);

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

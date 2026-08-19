//! `--healthcheck`: the container `HEALTHCHECK` command (distroless has no
//! shell/curl to run one). Exercises the real binary, not the `/livez`
//! handler directly, since the point under test is the CLI's exit code.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::process::Command;

/// Spawned via `spawn_blocking`: the harness's axum server runs as a task on
/// this same (current-thread, by default) test runtime, and a synchronous
/// `Command::status()` on the runtime thread would starve it of the chance
/// to accept the very connection this subprocess is about to make.
async fn run_healthcheck(port: u16) -> bool {
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_ruprogress-mcp"))
            .arg("--healthcheck")
            .env("SERVER_PORT", port.to_string())
            .status()
            .expect("binary should spawn")
            .success()
    })
    .await
    .expect("spawn_blocking should not panic")
}

fn port_of(base_url: &str) -> u16 {
    base_url
        .rsplit(':')
        .next()
        .and_then(|value| value.parse().ok())
        .expect("base_url should end in :<port>")
}

#[tokio::test]
async fn exits_zero_against_a_bound_server() {
    let harness = support::http_harness(&[]).await;
    assert!(run_healthcheck(port_of(&harness.base_url)).await);
}

#[tokio::test]
async fn exits_nonzero_against_a_closed_port() {
    let closed = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind to find a free port");
    let port = closed.local_addr().expect("local addr").port();
    drop(closed);

    assert!(!run_healthcheck(port).await);
}

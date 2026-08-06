//! Stdout-purity test: stdout is reserved for the
//! newline-delimited JSON-RPC stream. A stray `println!`/log line would
//! corrupt every MCP client talking to this server over stdio; this test
//! makes that regression a build/test failure by spawning the real binary
//! and asserting every non-empty stdout line parses as JSON.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;
use std::process::{Command, Stdio};

#[test]
fn every_stdout_line_from_the_real_binary_is_valid_json() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ruprogress-mcp"))
        .env("REDMINE_URL", "https://redmine.example.invalid")
        .env("REDMINE_API_KEY", "test-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary should spawn");

    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "stdout-purity-test", "version": "0.0.1" }
        }
    });

    {
        let stdin = child.stdin.as_mut().expect("child stdin should be piped");
        stdin
            .write_all(format!("{initialize}\n").as_bytes())
            .expect("write initialize frame");
    }
    // Close stdin so the server's read loop ends and the process exits.
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("child should exit");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    let mut saw_a_line = false;
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        saw_a_line = true;
        assert!(
            serde_json::from_str::<serde_json::Value>(line).is_ok(),
            "non-JSON line on stdout: {line:?}"
        );
    }
    assert!(
        saw_a_line,
        "expected at least one JSON-RPC response on stdout"
    );
}

/// `SIGTERM` shuts the server down cleanly (no panic, exit 0).
#[cfg(unix)]
#[test]
fn sigterm_shuts_the_server_down_cleanly() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ruprogress-mcp"))
        .env("REDMINE_URL", "https://redmine.example.invalid")
        .env("REDMINE_API_KEY", "test-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary should spawn");

    // Give the async runtime a moment to install the signal handler before
    // sending the signal.
    std::thread::sleep(std::time::Duration::from_millis(500));

    let status = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()
        .expect("kill command should run");
    assert!(status.success(), "failed to send SIGTERM");

    let exit_status = child.wait().expect("child should exit after SIGTERM");
    assert!(
        exit_status.success(),
        "expected a clean exit after SIGTERM, got {exit_status:?}"
    );
}

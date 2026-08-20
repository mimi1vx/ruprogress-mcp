//! `--print-config`: an operator feature and a directly testable secret-leak
//! assertion. The pure validation matrix over
//! `Config::from_map` lives as unit tests in `src/config.rs` — this file
//! covers what only the real binary can exercise: env-file loading and
//! stdout output, without ever touching this process's own environment.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `contents` to a uniquely named file under the OS temp dir, cleaned
/// up when the returned guard drops.
struct TempEnvFile(std::path::PathBuf);

impl TempEnvFile {
    fn new(contents: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ruprogress-mcp-test-env-{}-{id}",
            std::process::id()
        ));
        std::fs::write(&path, contents).expect("write temp env file");
        Self(path)
    }
}

impl Drop for TempEnvFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn print_config_reports_the_host_but_never_the_api_key() {
    const SECRET: &str = "super-secret-api-key-value";
    let env_file = TempEnvFile::new(&format!(
        "REDMINE_URL=https://redmine.example.com\nREDMINE_API_KEY={SECRET}\n"
    ));

    let output = Command::new(env!("CARGO_BIN_EXE_ruprogress-mcp"))
        .arg("--env-file")
        .arg(&env_file.0)
        .arg("--print-config")
        .env_remove("REDMINE_URL")
        .env_remove("REDMINE_API_KEY")
        .output()
        .expect("binary should run");

    assert!(output.status.success(), "exit status: {:?}", output.status);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert!(stdout.contains("redmine.example.com"), "stdout: {stdout}");
    assert!(
        !stdout.contains(SECRET),
        "stdout leaked the API key: {stdout}"
    );
}

#[test]
fn print_config_exits_nonzero_on_invalid_config() {
    let env_file = TempEnvFile::new("REDMINE_URL=not-a-url\n");

    let output = Command::new(env!("CARGO_BIN_EXE_ruprogress-mcp"))
        .arg("--env-file")
        .arg(&env_file.0)
        .arg("--print-config")
        .env_remove("REDMINE_URL")
        .env_remove("REDMINE_API_KEY")
        .output()
        .expect("binary should run");

    assert!(!output.status.success());
}

/// `--transport http --print-config` with `extra` appended to a minimal valid
/// env file.
fn print_http_config(extra: &str) -> std::process::Output {
    let env_file = TempEnvFile::new(&format!(
        "REDMINE_URL=https://redmine.example.com\nREDMINE_API_KEY=k\n{extra}"
    ));
    Command::new(env!("CARGO_BIN_EXE_ruprogress-mcp"))
        .arg("--env-file")
        .arg(&env_file.0)
        .args(["--transport", "http", "--print-config"])
        .env_clear()
        .output()
        .expect("binary should run")
}

#[test]
fn print_config_over_http_reports_the_bind_address() {
    let output = print_http_config("");
    assert!(output.status.success(), "exit status: {:?}", output.status);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(summary["transport"]["kind"], "http");
    assert_eq!(summary["transport"]["bind"], "127.0.0.1:8000");
    assert!(!stdout.contains("\"k\""), "stdout leaked the key: {stdout}");
}

#[test]
fn print_config_reports_legacy_per_user_auth_mode() {
    let output =
        print_http_config("REDMINE_AUTH_MODE=legacy-per-user\nREDMINE_PER_USER_TRUST_PROXY=true\n");
    assert!(output.status.success(), "exit status: {:?}", output.status);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(summary["auth_mode"], "legacy-per-user");
}

#[test]
fn a_non_loopback_bind_without_a_host_policy_refuses_to_start() {
    let output = print_http_config("SERVER_HOST=0.0.0.0\n");
    assert!(
        !output.status.success(),
        "a bare non-loopback bind must fail at startup, not serve with Host validation off"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    // The error message is the primary documentation for this failure; a
    // troubleshooting search lands on its exact text.
    assert!(stderr.contains("PUBLIC_HOST"), "stderr: {stderr}");
    assert!(
        stderr.contains("REDMINE_MCP_ALLOWED_HOSTS"),
        "stderr: {stderr}"
    );
}

#[test]
fn a_non_loopback_bind_starts_with_either_escape_hatch() {
    // PUBLIC_HOST is required in both cases: even the
    // REDMINE_MCP_ALLOWED_HOSTS=* escape hatch (which bypasses
    // parse_allowed_hosts's own PUBLIC_HOST requirement) still needs an
    // origin to build /files/{uuid} URLs from.
    for extra in [
        "SERVER_HOST=0.0.0.0\nPUBLIC_HOST=mcp.example.com\n\
         REDMINE_MCP_ALLOW_UNAUTHENTICATED_NETWORK=true\n",
        "SERVER_HOST=0.0.0.0\nPUBLIC_HOST=mcp.example.com\nREDMINE_MCP_ALLOWED_HOSTS=*\n\
         REDMINE_MCP_ALLOW_UNAUTHENTICATED_NETWORK=true\n",
    ] {
        let output = print_http_config(extra);
        assert!(
            output.status.success(),
            "should start with {extra:?}, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn a_non_loopback_bind_with_allowed_hosts_star_but_no_public_host_still_refuses_to_start() {
    // Building /files/{uuid} URLs needs an origin, which cannot be derived
    // from a non-loopback bind even when Host validation itself has been
    // disabled.
    let output = print_http_config("SERVER_HOST=0.0.0.0\nREDMINE_MCP_ALLOWED_HOSTS=*\n");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("PUBLIC_HOST"), "stderr: {stderr}");
}

/// Resolve config over the HTTP transport without binding anything, and
/// return the process's stderr. `--print-config` runs the whole of
/// `Config::from_map`, which is where the startup warnings are emitted.
fn config_stderr(extra: &str) -> String {
    let output = print_http_config(extra);
    String::from_utf8(output.stderr).expect("stderr should be UTF-8")
}

#[test]
fn a_non_loopback_bind_under_legacy_auth_warns_about_the_shared_key() {
    let stderr = config_stderr(
        "SERVER_HOST=0.0.0.0\nPUBLIC_HOST=mcp.example.com\n\
         REDMINE_MCP_ALLOW_UNAUTHENTICATED_NETWORK=true\n",
    );
    assert!(stderr.contains("WARN"), "stderr: {stderr}");
    assert!(
        stderr.contains("single shared Redmine API key"),
        "stderr: {stderr}"
    );
}

#[test]
fn a_non_loopback_bind_under_legacy_auth_without_the_override_refuses_to_start() {
    let output = print_http_config("SERVER_HOST=0.0.0.0\nPUBLIC_HOST=mcp.example.com\n");
    assert!(
        !output.status.success(),
        "a shared-key legacy server on a non-loopback bind must not start unattended"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(
        stderr.contains("REDMINE_MCP_ALLOW_UNAUTHENTICATED_NETWORK"),
        "stderr: {stderr}"
    );
}

#[test]
fn stdio_transport_is_unaffected_by_a_non_loopback_server_host() {
    let env_file = TempEnvFile::new(
        "REDMINE_URL=https://redmine.example.com\nREDMINE_API_KEY=k\nSERVER_HOST=0.0.0.0\n",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_ruprogress-mcp"))
        .arg("--env-file")
        .arg(&env_file.0)
        .args(["--transport", "stdio", "--print-config"])
        .env_clear()
        .output()
        .expect("binary should run");
    assert!(
        output.status.success(),
        "stdio has no bind, so SERVER_HOST is irrelevant; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn legacy_per_user_warns_about_the_trust_proxy_assumption() {
    let stderr =
        config_stderr("REDMINE_AUTH_MODE=legacy-per-user\nREDMINE_PER_USER_TRUST_PROXY=true\n");
    assert!(stderr.contains("WARN"), "stderr: {stderr}");
    assert!(stderr.contains("TLS-terminating proxy"), "stderr: {stderr}");
}

#[test]
fn disabling_host_validation_warns() {
    let stderr = config_stderr("REDMINE_MCP_ALLOWED_HOSTS=*\n");
    assert!(stderr.contains("WARN"), "stderr: {stderr}");
    assert!(
        stderr.contains("Host validation is disabled"),
        "stderr: {stderr}"
    );
}

#[test]
#[cfg(unix)]
fn the_effective_host_allowlist_is_logged_once_at_startup() {
    use std::process::Stdio;

    let env_file = TempEnvFile::new(
        "REDMINE_URL=https://redmine.example.com\nREDMINE_API_KEY=k\nSERVER_PORT=18322\n",
    );
    let child = Command::new(env!("CARGO_BIN_EXE_ruprogress-mcp"))
        .arg("--env-file")
        .arg(&env_file.0)
        .args(["--transport", "http"])
        .env_clear()
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary should start");

    std::thread::sleep(std::time::Duration::from_millis(750));
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    // `wait_with_output`, not `wait` then read: reading a pipe only after the
    // child exits deadlocks as soon as the boot log outgrows the pipe buffer.
    let output = child.wait_with_output().expect("child should exit");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    // A 403 must be diagnosable from the boot line alone, without
    // reconstructing the derivation from four environment variables.
    assert!(stderr.contains("allowed_hosts"), "stderr: {stderr}");
    assert!(stderr.contains("localhost"), "stderr: {stderr}");
    assert_eq!(
        stderr.matches("serving MCP over streamable HTTP").count(),
        1,
        "stderr: {stderr}"
    );
}

#[test]
#[cfg(unix)]
fn sigterm_drains_the_http_server_and_exits_zero() {
    use std::process::Stdio;

    let env_file = TempEnvFile::new(
        "REDMINE_URL=https://redmine.example.com\nREDMINE_API_KEY=k\nSERVER_PORT=18321\n",
    );
    let child = Command::new(env!("CARGO_BIN_EXE_ruprogress-mcp"))
        .arg("--env-file")
        .arg(&env_file.0)
        .args(["--transport", "http"])
        .env_clear()
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary should start");

    std::thread::sleep(std::time::Duration::from_millis(750));
    // `kill(2)` via the shell, so this crate stays `unsafe`-free.
    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("kill should run");
    assert!(killed.success());

    let output = child.wait_with_output().expect("child should exit");
    assert!(output.status.success(), "exit status: {:?}", output.status);
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("drained cleanly"), "stderr: {stderr}");
}

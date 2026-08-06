//! `--print-config`: an operator feature and a directly testable secret-leak
//! assertion. The pure validation matrix over
//! `Config::from_map` lives as unit tests in `src/config.rs` — this file
//! covers what only the real binary can exercise: env-file loading and
//! stdout output, without ever touching this process's own environment.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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

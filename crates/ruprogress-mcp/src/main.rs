//! CLI entry point: parse args, resolve config, run the stdio MCP server.
//! stdout is reserved for the JSON-RPC transport once the server starts —
//! everything else (tracing, `--print-config`) goes to stderr/a raw stdout
//! write that bypasses the `print_stdout` lint deliberately (see
//! `print_config` below).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Parser;
use redmine_client::{RedmineClient, RedmineClientBuilder};
use rmcp::ServiceExt as _;
use ruprogress_mcp::config::{AuthMode, Config, TransportConfig};
use ruprogress_mcp::server::RedmineMcp;

#[derive(Parser, Debug)]
#[command(name = "ruprogress-mcp")]
struct Cli {
    #[arg(long, value_enum, default_value_t = TransportKind::Stdio)]
    transport: TransportKind,
    #[arg(long)]
    env_file: Option<PathBuf>,
    #[arg(long)]
    log_level: Option<String>,
    /// Print the redacted resolved config and exit.
    #[arg(long)]
    print_config: bool,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum TransportKind {
    Stdio,
}

impl From<TransportKind> for TransportConfig {
    fn from(kind: TransportKind) -> Self {
        match kind {
            TransportKind::Stdio => Self::Stdio,
        }
    }
}

fn init_tracing(log_level: Option<&str>) {
    let filter = log_level.map_or_else(
        || std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        std::string::ToString::to_string,
    );
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
}

/// Load `.env`-style entries without ever mutating the process environment
/// (`dotenvy::dotenv()` would; edition 2024 makes `std::env::set_var`
/// `unsafe`, which this workspace forbids — see ADR 0002). Real environment
/// variables take precedence over the file.
fn load_env_map(env_file: Option<&Path>) -> anyhow::Result<BTreeMap<String, String>> {
    let file_entries = match env_file {
        Some(path) => {
            let iter = dotenvy::from_path_iter(path)
                .with_context(|| format!("failed to open --env-file {}", path.display()))?;
            iter.collect::<Result<Vec<_>, _>>()
                .with_context(|| format!("failed to parse --env-file {}", path.display()))?
        }
        None => dotenvy::dotenv_iter().map_or_else(
            |_| Ok(Vec::new()),
            |iter| {
                iter.collect::<Result<Vec<_>, _>>()
                    .context("failed to parse .env")
            },
        )?,
    };
    let mut vars: BTreeMap<String, String> = file_entries.into_iter().collect();
    vars.extend(std::env::vars());
    Ok(vars)
}

/// Bypasses `print!`/`println!` (denied by `clippy::print_stdout`) with a raw
/// stdout write: intentional, since the server has not started yet and this
/// is the one place a human-facing message belongs on stdout.
fn print_config(config: &Config) -> anyhow::Result<()> {
    let summary = serde_json::to_string_pretty(&config.redacted_summary())?;
    let mut stdout = std::io::stdout();
    stdout.write_all(summary.as_bytes())?;
    stdout.write_all(b"\n")?;
    Ok(())
}

fn build_redmine_client(config: &Config) -> anyhow::Result<RedmineClient> {
    let mut builder = RedmineClientBuilder::new(config.redmine.url.clone())
        .danger_accept_invalid_certs(!config.redmine.ssl_verify);
    if let AuthMode::Legacy { credential } = &config.auth {
        builder = builder.credential(credential.clone());
    }
    builder
        .build()
        .context("failed to build the Redmine client")
}

/// Waits for `SIGTERM`/`SIGINT`. Raced against the whole serve-and-wait
/// sequence in `main` (not just after it), because `RunningService::serve`
/// blocks until a client sends `initialize` — a signal handler installed
/// only after `.await`ing `serve()` would never see a signal that arrives
/// before a client connects, and the process would die to the *default*
/// signal disposition (uncaught, no graceful exit) instead.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn run_server(server: RedmineMcp) -> anyhow::Result<()> {
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .context("failed to start the stdio MCP transport")?;
    service.waiting().await.context("MCP server task failed")?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.log_level.as_deref());

    let vars = load_env_map(cli.env_file.as_deref())?;
    let transport = TransportConfig::from(cli.transport);
    let config = Config::from_map(&vars, transport)?;

    if cli.print_config {
        return print_config(&config);
    }

    let client = build_redmine_client(&config)?;
    let server = RedmineMcp::new(client, config);

    // Race a *spawned* task against the signal, not the future directly:
    // `serve()` reads from stdin on a blocking OS thread, which the same
    // SIGTERM can also interrupt (EINTR), nondeterministically finishing
    // that branch with a spurious transport error instead of letting the
    // signal branch win outright. Aborting the task on shutdown sidesteps
    // that race entirely — the process is about to exit either way.
    let mut handle = tokio::spawn(run_server(server));
    tokio::select! {
        result = &mut handle => match result {
            Ok(inner) => inner?,
            Err(join_err) if join_err.is_cancelled() => {}
            Err(join_err) => return Err(join_err.into()),
        },
        () = wait_for_shutdown_signal() => {
            tracing::info!("shutdown signal received, exiting");
            handle.abort();
        }
    }
    Ok(())
}

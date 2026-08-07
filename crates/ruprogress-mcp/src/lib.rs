//! MCP server library for `ruprogress-mcp`. `main.rs` is a thin CLI/bootstrap
//! wrapper; integration tests import this crate directly to build in-process
//! servers over `tokio::io::duplex` (see `tests/support/mod.rs`).

pub mod attachments;
pub mod auth;
pub mod config;
pub mod error;
mod health;
pub mod readonly;
pub mod render;
pub mod server;
pub mod tools;
pub mod transport;

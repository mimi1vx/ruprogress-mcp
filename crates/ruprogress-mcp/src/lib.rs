// The README is the crate docs, so any Rust example added to it becomes a
// doctest. `tests/readme_cli_flags.rs` pins its CLI table to `--help`.
#![doc = include_str!("../README.md")]
//! # Internals
//!
//! `main.rs` is a thin CLI/bootstrap wrapper around this library; integration
//! tests import the crate directly to build in-process servers over
//! `tokio::io::duplex` (see `tests/support/mod.rs`).

pub mod attachments;
pub mod auth;
pub mod config;
pub mod error;
mod health;
pub mod logging;
mod oauth;
mod panic_guard;
// `#[doc(hidden)] pub`, not private: `benches/ratelimit.rs` needs
// `ratelimit::Limiter`/`Decision`.
#[doc(hidden)]
pub mod ratelimit;
pub mod readonly;
pub mod render;
pub mod server;
pub mod tools;
pub mod transport;

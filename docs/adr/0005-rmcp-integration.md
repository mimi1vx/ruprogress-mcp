# ADR 0005: rmcp 3.1.1 integration — findings

## Context

Before writing `config.rs`/`server.rs`, three open questions needed answers:
whether `serve()` works over an in-process `tokio::io::duplex` pair (needed
for an e2e test harness), which `ProtocolVersion` `ServerInfo::new()`
defaults to, and whether enabling `rmcp`'s own `reqwest` feature alongside
`redmine-client`'s `reqwest` dependency would cause a rustls-provider
conflict.

Verified against the actual `rmcp` 3.1.1 crate contents (downloaded from
`static.crates.io`, not just its `main` branch) and the `rust-sdk`
repository's `examples/` and `tests/` on 2026-08-06.

## Findings

1. **`serve()` over `tokio::io::duplex` works, no extra feature needed.**
   `rmcp`'s own integration tests (`tests/test_tool_macros.rs`,
   `tests/test_server_initialization.rs`, etc.) call
   `server.serve(server_transport)` / `client_handler.serve(client_transport)`
   directly on `tokio::io::duplex(4096)` halves. This goes through the
   `(R, W): IntoTransport` impl gated by `transport-async-rw`, which the
   `server` feature already pulls in — `transport-io` (which adds
   `tokio/io-std` for real stdin/stdout) is *not* required for an in-process
   duplex harness. The e2e test harness needs no extra dev feature beyond
   `client` on `rmcp`.
2. **Default protocol version is `ProtocolVersion::LATEST`** (`V_2025_11_25`
   in this SDK snapshot), via `impl Default for ProtocolVersion`.
   `ServerInfo::new()`/`InitializeResult::new()` rely on this default rather
   than pinning `V_2024_11_05` as the upstream counter example does. We do
   not call `.with_protocol_version(...)` in `server.rs`.
3. **No rustls provider conflict.** `ruprogress-mcp` does not enable rmcp's
   own `reqwest` feature (`rmcp/reqwest`) — only `server`, `macros`,
   `transport-io`, and (dev) `client`. All Redmine HTTP traffic goes through
   `redmine-client`'s own `reqwest` dependency (`rustls` feature, no rmcp
   involvement). There is therefore no second `reqwest`/rustls provider in
   the dependency graph from rmcp's side; `cargo tree -d` after adding the
   dependency shows a single `reqwest` resolution.

## Verified rmcp 3.1.1 API shape

Confirmed against the actual 3.1.1 crate contents:

- `#[tool_handler]` takes only an optional `meta = ...`; identity/instructions
  come from `ServerHandler::get_info() -> ServerInfo`.
- `ToolRouter::{remove_route, disable_route, has_route, list_all}` all exist;
  `remove_route` deletes the entry entirely (`self.map.remove(name)`), so
  both `tools/list` and `tools/call` ("tool not found", `invalid_params`) are
  affected — one choke point for read-only mode.
- `ContentBlock::text(...)` and `CallToolResult::success(...)` match the
  upstream counter example.
- `Implementation::from_build_env()` is the default `server_info` on
  `InitializeResult::new`.

One subtlety not obvious from the docs, found while implementing graceful
shutdown: `ServiceExt::serve()` for a server **blocks until the client sends
`initialize`** (`serve_server_with_ct_inner` loops on `expect_next_message`
before returning a `RunningService`). A signal handler installed only after
`.await`ing `serve()` — e.g. via `RunningService::cancellation_token()`, the
first approach tried here — never sees a `SIGTERM` that arrives before a
client connects, and the process dies to the *default* signal disposition
instead of exiting cleanly. Worse, once a handler *is* installed, the same
`SIGTERM` can independently interrupt the blocking OS thread underneath
`tokio::io::stdin()`'s read, nondeterministically finishing `serve()`'s
future with a spurious "connection closed" transport error in a race with
the signal future — flipping the exit code between 0 and 1 from one run to
the next.

`main.rs` avoids both failure modes by `tokio::spawn`-ing the serve-and-wait
sequence and racing the **`JoinHandle`** (not the bare future) against
`wait_for_shutdown_signal()` in a `tokio::select!`, aborting the handle on
the signal branch. This makes the outcome depend only on which of "signal
arrived" vs. "server task naturally finished" happened first, independent of
whichever thread the same OS signal also happened to interrupt.

## Decision

Use `rmcp = "=3.1.1"`, features `server`, `macros`, `transport-io` (binary),
dev-feature `client` (tests only, via Cargo's per-target feature unification
under `resolver = "3"`). No workaround or version bump needed.

## Addition: `src/lib.rs`

The e2e test harness (`tests/support/mod.rs`) needs to construct `Config`,
`RedmineMcp`, etc. from outside the crate. A `tests/` integration test can
only do that against a **library** target — a `main.rs`-only binary crate
exposes nothing to import. Adding `src/lib.rs` (`pub mod config; pub mod
server; ...`) with `main.rs` reduced to a thin CLI/bootstrap wrapper resolves
this; it is the standard bin+lib pattern and does not add a new external
dependency or public API surface (nothing publishes this crate).

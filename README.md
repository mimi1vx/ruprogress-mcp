# ruprogress-mcp

A Redmine MCP server in Rust, providing tool-parity with the reference
[jztan/redmine-mcp-server](https://github.com/jztan/redmine-mcp-server)
(Python/FastMCP).

**Status: not yet usable.** This repository is in early scaffolding
(workspace, lints, CI). No tools are implemented yet.

## Scope

- Full tool parity with the reference server (~51 tools).
- Both stdio and streamable HTTP transports.
- Four auth modes: `legacy`, `legacy-per-user`, `oauth`, `oauth-proxy`.
- A reusable `redmine-client` crate, independent of MCP.

See `docs/adr/` for the design decisions made along the way.

## Non-goals (v1.0)

- Interactive Apps tools (drag-and-drop dashboards) — deferred until `rmcp`
  supports the MCP Apps extension.
- Horizontal scaling / shared `oauth-proxy` state — single-process only.

## Development

```sh
cargo build --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

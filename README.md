# ruprogress-mcp

A Redmine MCP server in Rust, providing tool-parity with the reference
[jztan/redmine-mcp-server](https://github.com/jztan/redmine-mcp-server)
(Python/FastMCP).

**Status: early.** Three read-only tools (`get_mcp_server_info`,
`get_current_user`, `list_redmine_projects`) over both stdio and streamable
HTTP. The remaining ~48 tools are not implemented yet.

## Quick start

```sh
cp .env.example .env      # set REDMINE_URL and REDMINE_API_KEY
cargo run -- --print-config          # resolve and check the config
cargo run                            # stdio (the default)
cargo run -- --transport http        # http on 127.0.0.1:8000
```

Point MCP Inspector at it:

```sh
npx @modelcontextprotocol/inspector --cli http://127.0.0.1:8000/mcp \
  --transport http --method tools/list
```

The HTTP server also exposes `/livez`, `/readyz`, and `/health`. It binds
loopback by default; see
[Exposing the server on a network](docs/configuration.md#exposing-the-server-on-a-network)
before changing `SERVER_HOST`.

## Scope

- Full tool parity with the reference server (~51 tools).
- Both stdio and streamable HTTP transports.
- Four auth modes: `legacy` and `legacy-per-user` implemented (the latter
  documented in `docs/legacy-per-user-auth.md`); `oauth`/`oauth-proxy` not
  yet.
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

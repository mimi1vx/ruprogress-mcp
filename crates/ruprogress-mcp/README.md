# ruprogress-mcp

A Redmine MCP server in Rust. Exposes Redmine's REST API to MCP clients as 41
tools over stdio and streamable HTTP (47 with every plugin flag on), with four
authentication modes, a read-only mode, bounded responses, and a local
attachment store.

```sh
cargo install ruprogress-mcp
ruprogress-mcp --print-config   # resolve and check the config, then exit
```

Or run the container image — a distroless, non-root, multi-arch (amd64/arm64)
build published on every release:

```sh
docker run --rm -p 8000:8000 \
  -e REDMINE_URL=https://redmine.example.com \
  -e REDMINE_API_KEY=... \
  ghcr.io/mimi1vx/ruprogress-mcp:latest --transport http
```

## Configuration

Two variables are required; everything else has a default. `REDMINE_URL` is
the base URL of the Redmine instance, `REDMINE_API_KEY` the key the server
authenticates with. They can come from the environment or from an `.env` file
(`--env-file`, or `.env` if present), with the real environment winning.
Wiring it into an MCP client over stdio:

```json
{
  "mcpServers": {
    "redmine": {
      "command": "ruprogress-mcp",
      "env": {
        "REDMINE_URL": "https://redmine.example.com",
        "REDMINE_API_KEY": "..."
      }
    }
  }
}
```

| Flag | Default | Effect |
|---|---|---|
| `--transport <stdio\|http>` | `stdio` | Which transport to serve. |
| `--env-file <PATH>` | `.env` if present | Env file to load; the real process environment still wins. |
| `--log-level <FILTER>` | `RUST_LOG`, else `info` | Tracing filter. |
| `--print-config` | — | Print the resolved, redacted config as JSON and exit 0. |
| `--healthcheck` | — | `GET /livez` on the local `SERVER_PORT` and exit 0/1; the container `HEALTHCHECK`. |

The HTTP transport binds `127.0.0.1:8000` and serves `/mcp` (stateless
streamable HTTP) plus `/livez`, `/readyz`, and `/files/{uuid}`. Changing
`SERVER_HOST` to a non-loopback address refuses to start without an explicit
opt-in — see
[Exposing the server on a network](https://github.com/mimi1vx/ruprogress-mcp/blob/main/docs/configuration.md#exposing-the-server-on-a-network).

## Tools

Issues, projects, versions, members, categories, relations, time entries,
wiki pages, queries, search, Gantt, and attachments — plus plugin families
gated behind `REDMINE_CHECKLISTS_ENABLED`, `REDMINE_PRODUCTS_ENABLED`,
`REDMINE_CRM_ENABLED`, and `REDMINE_DMSF_ENABLED`, and an admin-gated
`cleanup_attachment_files`. `REDMINE_AGILE_ENABLED`/`REDMINE_TAGS_ENABLED` add
no new tools — they unlock extra parameters on the existing issue tools.

Every tool returns structured JSON with a declared `outputSchema`. Failures
come back in-band as `{error, code, retryable, hint}` with a stable `code`
(`NOT_FOUND`, `VALIDATION_FAILED`, `READ_ONLY`, `CONFIRMATION_REQUIRED`,
`INSUFFICIENT_SCOPE`, `INTERNAL`, …) rather than as MCP protocol errors, so a
model can act on them.

## Built for untrusted callers

- **Four auth modes.** `legacy` (one shared API key), `legacy-per-user` (each
  request carries its caller's own key), `oauth` (Redmine Doorkeeper bearer
  tokens validated by RFC 7662 introspection, with per-tool scope
  enforcement), and `oauth-proxy` (this server is itself an authorization
  server: RFC 7591 dynamic client registration plus authorization-code +
  PKCE).
- **Read-only mode.** `REDMINE_MCP_READ_ONLY=true` removes every write tool
  from the router outright and refuses the write actions of the mixed
  read/write tools with `code: "READ_ONLY"`.
- **Guarded destructive tools.** Deleting an issue refuses without
  `confirm_delete=true` and returns an impact preview first; bulk operations
  are bounded.
- **Bounded responses.** List results are capped by item count and byte size,
  and a truncated payload says so rather than being silently cut.
- **Prompt-injection fencing.** Text drawn from Redmine is wrapped in a
  per-response random-nonce fence, and the session `instructions` tell the
  model that fenced content is data, not instructions.
- **Rate limiting, panic containment, and log hygiene.** Per-class token
  buckets on the HTTP transport, a panicking tool handler answered with
  `INTERNAL` instead of left hanging, and a payload-safety floor that keeps
  dependency logs from printing tool arguments or wire bodies.

## Documentation

- [Repository README](https://github.com/mimi1vx/ruprogress-mcp) — the full
  tool table, HTTP endpoints, and Docker usage.
- [docs/configuration.md](https://github.com/mimi1vx/ruprogress-mcp/blob/main/docs/configuration.md)
  — every environment variable.
- [docs/tool-reference.md](https://github.com/mimi1vx/ruprogress-mcp/blob/main/docs/tool-reference.md)
  — generated from the live schemas: every tool's parameters, kind, and scopes.
- [docs/oauth-setup.md](https://github.com/mimi1vx/ruprogress-mcp/blob/main/docs/oauth-setup.md)
  — Doorkeeper setup for `oauth` and `oauth-proxy`.
- [docs/troubleshooting.md](https://github.com/mimi1vx/ruprogress-mcp/blob/main/docs/troubleshooting.md)
  — what each error code means and what to do about it.

The Redmine HTTP layer is a separate crate,
[`redmine-client`](https://crates.io/crates/redmine-client), usable without
any MCP dependency.

## Requirements

Rust 1.96 (edition 2024). MIT licensed.

# ruprogress-mcp

A Redmine MCP server in Rust. It exposes Redmine's REST API to MCP clients as
41 tools over stdio and streamable HTTP, with three authentication modes, a
read-only mode, bounded responses, and a local attachment store.

## Quick start

```sh
cp .env.example .env                 # set REDMINE_URL and REDMINE_API_KEY
cargo run -- --print-config          # resolve and check the config, then exit
cargo run                            # stdio (the default)
cargo run -- --transport http        # http on 127.0.0.1:8000
```

Point MCP Inspector at it:

```sh
npx @modelcontextprotocol/inspector --cli http://127.0.0.1:8000/mcp \
  --transport http --method tools/list
```

### CLI

| Flag | Default | Effect |
|---|---|---|
| `--transport <stdio\|http>` | `stdio` | Which transport to serve. |
| `--env-file <PATH>` | `.env` if present | Env file to load; the real process environment still wins. |
| `--log-level <FILTER>` | `RUST_LOG`, else `info` | Tracing filter. |
| `--print-config` | — | Print the resolved, redacted config as JSON and exit 0. |

Everything else is environment-driven — see [docs/configuration.md](docs/configuration.md).

## Tools

41 tools are registered by default; `cleanup_attachment_files` brings the
total to 42 when `REDMINE_MCP_EXPOSE_ADMIN_TOOLS=true`. Parameters and return
shapes are in [docs/tool-contract.md](docs/tool-contract.md).

| Family | Tools |
|---|---|
| Meta | `get_mcp_server_info` |
| Discovery | `list_redmine_trackers`, `list_project_trackers`, `list_redmine_issue_statuses`, `list_redmine_issue_priorities`, `list_redmine_users`, `get_current_user`, `list_redmine_queries` |
| Projects | `list_redmine_projects`, `list_project_issue_custom_fields`, `summarize_project_status`, `list_redmine_versions`, `manage_redmine_version`, `list_project_members`, `list_redmine_roles`, `get_project_modules`, `manage_project_member` |
| Issues (read) | `get_redmine_issue`, `list_redmine_issues`, `search_redmine_issues`, `list_subtasks`, `get_private_notes` |
| Issues (write) | `create_redmine_issue`, `update_redmine_issue`, `delete_redmine_issue`, `copy_issue`, `manage_issue_note`, `manage_issue_watcher` |
| Relations & categories | `manage_issue_relation`, `manage_issue_category` |
| Time tracking | `list_time_entries`, `manage_time_entry`, `list_time_entry_activities`, `import_time_entries` |
| Search & wiki | `search_entire_redmine`, `manage_redmine_wiki_page` |
| Gantt | `get_gantt_chart` |
| Files | `get_redmine_attachment`, `list_files`, `upload_file`, `delete_file`, `cleanup_attachment_files` (admin-gated) |

Destructive tools are guarded rather than merely annotated:
`delete_redmine_issue` refuses without `confirm_delete=true` and returns an
impact preview first, `delete_file` requires
`confirm_delete_any_attachment=true`, `copy_issue` is bounded to 50 issues, and
`import_time_entries` to 500 entries.

### Tool output

Every tool returns structured JSON content with a declared `outputSchema`.
Failures come back in-band as `{error, code, retryable, hint}` with a stable
`code` (`NOT_FOUND`, `VALIDATION_FAILED`, `RATE_LIMITED`, `READ_ONLY`,
`CONFIRMATION_REQUIRED`, `INSUFFICIENT_SCOPE`, …) rather than as MCP protocol
errors, so a model can act on them.

List responses are capped by `REDMINE_MCP_MAX_RESPONSE_ITEMS` (200) and
`REDMINE_MCP_MAX_RESPONSE_BYTES` (256 KiB); a truncated payload says so via
`pagination.truncated` and a `pagination.hint`, never a silent cut.

Text drawn from Redmine (subjects, descriptions, journal notes, wiki bodies) is
wrapped in a per-response random-nonce fence and the session `instructions`
explain that fenced content is data, not instructions.

### Schema dialect

`REDMINE_MCP_SCHEMA_DIALECT=portable` rewrites every `inputSchema` to inline
`$ref`/`$defs` and collapse `{"type":["T","null"]}` to `{"type":"T"}`, for
clients (Google Vertex/Gemini) whose function-calling validator rejects the
full JSON Schema 2020-12 form. The default `strict` keeps the rich form. See
[ADR 0007](docs/adr/0007-json-schema-format-normalization.md).

## Authentication

`REDMINE_AUTH_MODE` selects one of three modes:

| Mode | Credential | Transport |
|---|---|---|
| `legacy` (default) | One `REDMINE_API_KEY` shared by every client. | stdio, http |
| `legacy-per-user` | Each request carries its caller's own `X-Redmine-API-Key`. | http only |
| `oauth` | Each client presents a Redmine Doorkeeper bearer token, validated by RFC 7662 introspection and forwarded upstream verbatim. | http only |

`legacy-per-user` requires the operator to attest to a TLS-terminating proxy
via `REDMINE_PER_USER_TRUST_PROXY=true` — see
[docs/legacy-per-user-auth.md](docs/legacy-per-user-auth.md).

`oauth` mode additionally serves RFC 9728 protected-resource metadata, RFC 8414
authorization-server metadata, and an RFC 7009 `POST /revoke` proxy, and
enforces per-tool scopes: `tools/list` shows only what a token's scopes permit
and `tools/call` on anything else is refused with `INSUFFICIENT_SCOPE`. See
[docs/oauth-setup.md](docs/oauth-setup.md).

Setting `REDMINE_MCP_READ_ONLY=true` removes the 12 write-only tools from the
router entirely and makes the write actions of `manage_issue_relation`,
`manage_issue_category`, and `manage_redmine_wiki_page` refuse with
`code: "READ_ONLY"`.

## HTTP transport

| Path | Purpose |
|---|---|
| `/mcp` (`FASTMCP_STREAMABLE_HTTP_PATH`) | Streamable HTTP MCP endpoint, stateless. |
| `/livez` | Process liveness; never touches Redmine. |
| `/readyz` | TTL-cached Redmine reachability probe. |
| `/health` | Alias for `/readyz`. |
| `/files/{uuid}` | Downloads a staged attachment. |
| `/.well-known/oauth-protected-resource…`, `/.well-known/oauth-authorization-server…`, `/revoke` | `oauth` mode only. |

The edge is hardened: a `Host` allowlist against DNS rebinding (applied to both
`/mcp` and `/files`), exact-match CORS only when origins are configured, a
streamed request-body cap, and `X-Content-Type-Options: nosniff` everywhere.

It binds loopback by default; read
[Exposing the server on a network](docs/configuration.md#exposing-the-server-on-a-network)
before changing `SERVER_HOST`.

## Attachments

Downloaded attachments are staged in a local store (`ATTACHMENTS_DIR`, created
`0700`, defaulting under the temp dir) behind opaque UUIDs, with a per-file cap
(200 MiB), a whole-store cap (2 GiB), and a background sweeper that expires
files after `ATTACHMENT_EXPIRES_MINUTES`. `get_redmine_attachment` returns a
`/files/{uuid}` URL on the HTTP transport and an absolute `file_path` on stdio.

`upload_file` accepts `content_base64` or `file_path`; a `file_path` must
resolve inside `ATTACHMENTS_DIR` or a `REDMINE_MCP_UPLOAD_FILE_ROOTS` entry, or
it is refused with `PATH_NOT_ALLOWED`. `create_redmine_issue` and
`update_redmine_issue` attach files in the same call, and `REDMINE_PUBLIC_URL`
rewrites every emitted `content_url` to a client-reachable origin.

## Layout

- `crates/redmine-client` — a typed Redmine REST client with retries, TLS
  options, and pagination. Independent of MCP and reusable on its own.
- `crates/ruprogress-mcp` — the MCP server and binary.
- `docs/adr/` — the design decisions made along the way.

## Non-goals (v1.0)

- Interactive Apps tools (drag-and-drop dashboards) — deferred until `rmcp`
  supports the MCP Apps extension.
- `oauth-proxy` mode, with this server acting as an authorization server doing
  Dynamic Client Registration.
- Plugin tool families (checklists, products, CRM contacts, DMSF documents);
  their `REDMINE_*_ENABLED` flags are reported by `get_mcp_server_info` only.
- Horizontal scaling / shared auth state — single-process only.

## Development

```sh
cargo build --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

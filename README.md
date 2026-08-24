# ruprogress-mcp

A Redmine MCP server in Rust. It exposes Redmine's REST API to MCP clients as
41 tools over stdio and streamable HTTP (47 with every plugin flag on), with
four authentication modes, a read-only mode, bounded responses, and a local
attachment store.

## Quick start

```sh
cp .env.example .env                 # set REDMINE_URL and REDMINE_API_KEY
cargo run -- --print-config          # resolve and check the config, then exit
cargo run                            # stdio (the default)
cargo run -- --transport http        # http on 127.0.0.1:8000
```

Or install the published crate, or run the container image:

```sh
cargo install ruprogress-mcp
docker run --rm -p 8000:8000 \
  -e REDMINE_URL=https://redmine.example.com -e REDMINE_API_KEY=... \
  ghcr.io/mimi1vx/ruprogress-mcp:latest --transport http
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

41 tools are registered by default. `cleanup_attachment_files`
(`REDMINE_MCP_EXPOSE_ADMIN_TOOLS=true`) and the plugin-gated families below
add more on top, each independently: `REDMINE_CHECKLISTS_ENABLED` adds 3,
`REDMINE_PRODUCTS_ENABLED`/`REDMINE_CRM_ENABLED`/`REDMINE_DMSF_ENABLED` add
one each. `REDMINE_AGILE_ENABLED`/`REDMINE_TAGS_ENABLED` add no new tools —
they unlock extra parameters on the existing issue tools instead. See
[docs/tool-reference.md](docs/tool-reference.md) (generated, always current)
for every tool's parameters, and
[docs/tool-contract.md](docs/tool-contract.md) for the narrative contract.

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
| Plugins (flag-gated) | `get_checklist`, `create_checklist_item`, `update_checklist_item` (`REDMINE_CHECKLISTS_ENABLED`), `manage_product` (`REDMINE_PRODUCTS_ENABLED`), `manage_contact` (`REDMINE_CRM_ENABLED`), `manage_document` (`REDMINE_DMSF_ENABLED`) |

Destructive tools are guarded rather than merely annotated:
`delete_redmine_issue` refuses without `confirm_delete=true` and returns an
impact preview first, `delete_file` requires
`confirm_delete_any_attachment=true`, `copy_issue` is bounded to 50 issues, and
`import_time_entries` to 500 entries.

### Tool output

Every tool returns structured JSON content with a declared `outputSchema`.
Failures come back in-band as `{error, code, retryable, hint}` with a stable
`code` (`NOT_FOUND`, `VALIDATION_FAILED`, `RATE_LIMITED`, `READ_ONLY`,
`CONFIRMATION_REQUIRED`, `INSUFFICIENT_SCOPE`, `INTERNAL`, …) rather than as
MCP protocol errors, so a model can act on them. A panicking tool handler is
caught and answered with `code: "INTERNAL"` rather than left hanging.

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

`REDMINE_AUTH_MODE` selects one of four modes:

| Mode | Credential | Transport |
|---|---|---|
| `legacy` (default) | One `REDMINE_API_KEY` shared by every client. | stdio, http |
| `legacy-per-user` | Each request carries its caller's own `X-Redmine-API-Key`. | http only |
| `oauth` | Each client presents a Redmine Doorkeeper bearer token, validated by RFC 7662 introspection and forwarded upstream verbatim. | http only |
| `oauth-proxy` | This server is itself an authorization server: clients register via RFC 7591 DCR and run authorization-code + PKCE against `/authorize`/`/token`, which drive a second flow against Redmine on their behalf. | http only |

`legacy-per-user` requires the operator to attest to a TLS-terminating proxy
via `REDMINE_PER_USER_TRUST_PROXY=true` — see
[docs/legacy-per-user-auth.md](docs/legacy-per-user-auth.md).

`oauth` mode additionally serves RFC 9728 protected-resource metadata, RFC 8414
authorization-server metadata, and an RFC 7009 `POST /revoke` proxy, and
enforces per-tool scopes: `tools/list` shows only what a token's scopes permit
and `tools/call` on anything else is refused with `INSUFFICIENT_SCOPE`. See
[docs/oauth-setup.md](docs/oauth-setup.md).

Setting `REDMINE_MCP_READ_ONLY=true` removes the always-write tools from the
router entirely and makes the write actions of `manage_issue_relation`,
`manage_issue_category`, `manage_redmine_wiki_page`, `manage_product`,
`manage_contact`, and `manage_document` refuse with `code: "READ_ONLY"`.

## HTTP transport

| Path | Purpose |
|---|---|
| `/mcp` (`FASTMCP_STREAMABLE_HTTP_PATH`) | Streamable HTTP MCP endpoint, stateless. |
| `/livez` | Process liveness; never touches Redmine. |
| `/readyz` | TTL-cached Redmine reachability probe. |
| `/health` | Alias for `/readyz`. |
| `/files/{uuid}` | Downloads a staged attachment. |
| `/.well-known/oauth-protected-resource…`, `/.well-known/oauth-authorization-server…` | `oauth`/`oauth-proxy` modes only. |
| `/revoke` | `oauth`/`oauth-proxy` modes only, with mode-specific semantics — see `docs/oauth-setup.md`. |
| `/register`, `/authorize`, `/auth/callback`, `/token` | `oauth-proxy` mode only: RFC 7591 DCR, the authorization-code + PKCE flow, and the `refresh_token` grant. |

The edge is hardened: a `Host` allowlist against DNS rebinding (applied to both
`/mcp` and `/files`), exact-match CORS only when origins are configured, a
streamed request-body cap, and `X-Content-Type-Options: nosniff` everywhere.

It binds loopback by default; read
[Exposing the server on a network](docs/configuration.md#exposing-the-server-on-a-network)
before changing `SERVER_HOST` — in the default `legacy` auth mode a
non-loopback bind refuses to start without an explicit opt-in.

### Rate limiting

`REDMINE_MCP_RATE_LIMIT_ENABLED` (default `true`) applies a token bucket per
class: a standard class on `/mcp`/`/files/{uuid}` (`REDMINE_MCP_RATE_LIMIT_RPS`/
`_BURST`, default 10 rps / burst 40), and a stricter class on the
`oauth-proxy` flow routes (`REDMINE_MCP_RATE_LIMIT_AUTH_RPS`/`_BURST`,
default 1 rps / burst 10). Both key by peer IP — never `X-Forwarded-For`/
`X-Real-IP`, which a client could set itself — except the standard class,
which keys `/mcp` by bearer-token digest instead when a token is present, so
distinct users behind one NAT or proxy don't share a bucket. A rejected
request gets `429 {"error": "rate_limited"}` with `Retry-After`; `/livez`,
`/readyz`, and `/health` are never rate limited. See
[docs/troubleshooting.md](docs/troubleshooting.md) for the reverse-proxy
caveat this IP-keying implies.

## Docker

A distroless, non-root image, built locally (no registry push, no multi-arch
matrix). On Apple silicon, pin the build to `arm64` explicitly — an unpinned
build silently produces `amd64` under emulation:

```sh
docker build --platform linux/arm64 -t ruprogress-mcp:dev .
```

```sh
docker run --rm -p 8000:8000 \
  -e REDMINE_URL=https://redmine.example.com \
  -e REDMINE_API_KEY=... \
  -e PUBLIC_HOST=localhost \
  -e REDMINE_MCP_ALLOW_UNAUTHENTICATED_NETWORK=true \
  ruprogress-mcp:dev
```

The image already sets `SERVER_HOST=0.0.0.0`; `PUBLIC_HOST` is still required
(see [Exposing the server on a network](docs/configuration.md#exposing-the-server-on-a-network)) —
without it the container exits immediately with a `Missing PUBLIC_HOST` error.
The default `legacy` auth mode additionally requires
`REDMINE_MCP_ALLOW_UNAUTHENTICATED_NETWORK=true` as an explicit attestation
that a single shared API key on a non-loopback bind is acceptable here — the
`-p` mapping (or `docker-compose.yml`'s loopback-only publication) is what
actually limits reach. `curl -f localhost:8000/livez` should then return `200`.

For a locked-down run — read-only root filesystem, a named volume for the one
directory the process writes to — see `docker-compose.yml`, or by hand:

```sh
docker volume create ruprogress-mcp-attachments
docker run --rm -p 8000:8000 \
  -e REDMINE_URL=https://redmine.example.com \
  -e REDMINE_API_KEY=... \
  -e PUBLIC_HOST=localhost \
  -e REDMINE_MCP_ALLOW_UNAUTHENTICATED_NETWORK=true \
  --read-only \
  -v ruprogress-mcp-attachments:/var/lib/ruprogress-mcp/attachments \
  ruprogress-mcp:dev
```

The image's own `HEALTHCHECK` runs `ruprogress-mcp --healthcheck` against
`/livez` (distroless has no shell or `curl` to run one another way) and never
`/readyz` — a Redmine outage must not turn into a container restart loop. Point
an orchestrator's own readiness probe at `/readyz` separately.

## Logging

`--log-level` (falling back to `RUST_LOG`, default `info`) is a standard
`tracing_subscriber::EnvFilter` string, written to stderr — stdout is
reserved for the JSON-RPC stream on the `stdio` transport, enforced by a
deny-level clippy lint and an end-to-end test that parses every stdout line
as JSON. Whatever is requested is combined with a **payload-safety floor**
that caps `rmcp`/`hyper`/`h2`/`reqwest`/`rustls`/`wiremock` at `info` (this
server's own code is never floored), since those dependencies' own
`DEBUG`/`TRACE` output can include a full tool call's arguments or wire-level
bodies. `REDMINE_MCP_LOG_FORMAT` (`text`, default, or `json`) only changes
how a line is written, never what is in it.

Every `tools/call`, on both transports, opens one span and closes it with a
single event carrying the tool name, a process-local `request_id`, `outcome`
(`ok`/`error`/`denied`/`panic`), a `code` when not `ok`, and `duration_ms` —
never an argument value or key. See
[docs/configuration.md](docs/configuration.md#logging) for the full
rationale, including what "redaction" does and does not mean here.

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
- `upload_file`/`manage_document`'s `source_url`: fetching a caller-supplied
  URL server-side, pending a decision on the SSRF exposure it would add.
- Horizontal scaling / shared auth state — single-process only.

## Development

```sh
cargo build --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

## Releasing

Versions, the changelog, tags, GitHub Releases, and both crates.io
publishes are automated — see [docs/releasing.md](docs/releasing.md).

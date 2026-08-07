# Configuration

Environment variable names are vendored from the upstream reference server
(`jztan/redmine-mcp-server`, branch `develop`, `.env.example`, captured
2026-08-06) so existing `.env` files port unchanged. Only the variables
`ruprogress-mcp` currently reads are validated; the rest are recorded here
for later and are currently ignored.

Config is loaded via `Config::from_map`, a pure function over an injected
`BTreeMap<String, String>` — never the ambient environment directly (see ADR
0002). `main.rs` builds that map from, in increasing precedence: an optional
`--env-file` (or `.env` if present and `--env-file` is not given), then the
real process environment.

## Currently implemented

| Variable | Required | Default | Notes |
|---|---|---|---|
| `REDMINE_URL` | yes | — | Must be `http`/`https` and must not contain userinfo (`https://user:pass@host` is rejected — credentials belong in `REDMINE_API_KEY`, not the URL). |
| `REDMINE_AUTH_MODE` | no | `legacy` | One of `legacy`, `legacy-per-user`, `oauth`. Any other value is `Invalid`. |
| `REDMINE_API_KEY` | yes, in `legacy` mode | — | The API key sent as `X-Redmine-API-Key`. Mutually exclusive with `REDMINE_API_KEY_FILE`. |
| `REDMINE_API_KEY_FILE` | yes, in `legacy` mode (alternative) | — | Path to a file containing the key (Docker/K8s secret mount). Exactly one trailing newline is trimmed. Setting both this and `REDMINE_API_KEY` is a `Conflict`. |
| `REDMINE_PER_USER_TRUST_PROXY` | yes, in `legacy-per-user` mode | — | Must be exactly `true`. Absent/false refuses to start (`Missing`) — this is the operator's explicit attestation that a TLS-terminating proxy sits in front and does not forward client `X-Forwarded-Proto`. `legacy-per-user` is additionally rejected outright on the `stdio` transport (`Conflict`): there is no per-request header to carry a credential over stdio. |
| `REDMINE_PER_USER_AUDIT_IDENTITY` | no | `false` | Reserved for a future per-user-auth identity audit feature; parsed now so the config surface is stable. |
| `REDMINE_MCP_BASE_URL` | yes, in `oauth` mode | — | Public base URL of this MCP server. Full OAuth wiring is not implemented yet; only presence is validated here. |
| `REDMINE_SSL_VERIFY` | no | `true` | `false` is accepted but logs a `WARN` — never silently downgrades without a trace. |
| `REDMINE_MCP_READ_ONLY` | no | `false` | Removes every tool in `readonly::write_tools::ALL` from the router (hides from `tools/list` **and** rejects `tools/call`). |
| `REDMINE_MCP_SCHEMA_DIALECT` | no | `strict` | One of `strict`, `portable`. `portable` inlines every `inputSchema`'s `$ref`/`$defs` and collapses `{"type":["T","null"]}` to `{"type":"T"}`, for clients (Google Vertex/Gemini) whose function-calling schema validator rejects the rich JSON Schema 2020-12 form. `outputSchema` is unaffected either way — see ADR 0007. |
| `REDMINE_AGILE_ENABLED` | no | `false` | Surfaced in `plugin_flags.agile` (`get_mcp_server_info`). No agile tools exist yet. |
| `REDMINE_CHECKLISTS_ENABLED` | no | `false` | `plugin_flags.checklists`. |
| `REDMINE_PRODUCTS_ENABLED` | no | `false` | `plugin_flags.products`. |
| `REDMINE_CRM_ENABLED` | no | `false` | `plugin_flags.crm`. |
| `REDMINE_DMSF_ENABLED` | no | `false` | `plugin_flags.dmsf`. |
| `REDMINE_TAGS_ENABLED` | no | `false` | `plugin_flags.tags`. |

### HTTP transport (`--transport http` only)

Ignored entirely on `--transport stdio`.

| Variable | Required | Default | Notes |
|---|---|---|---|
| `SERVER_HOST` | no | `127.0.0.1` | Must be an **IP literal** (`127.0.0.1`, `::1`, `0.0.0.0`); hostnames are rejected so the bound interface never depends on DNS. A non-loopback value requires a Host policy — see "Exposing the server on a network". |
| `SERVER_PORT` | no | `8000` | 1–65535. `0` is rejected: the server would bind a port no client could be told about. |
| `PUBLIC_HOST` | yes, for a non-loopback `SERVER_HOST` | — | The hostname clients use to reach this server. Added to the `Host` allowlist bare, which matches the host on **any** port. |
| `PUBLIC_PORT` | no | — | Pins the `PUBLIC_HOST` entry to one port (`host:port` instead of bare `host`). Setting it alone is a `Conflict`. |
| `FASTMCP_STREAMABLE_HTTP_PATH` | no | `/mcp` | Must start with `/`, have at least one segment, and contain no `..`, `?`, `#`, `{`, `}`, `*`, or whitespace. |
| `REDMINE_MCP_ALLOWED_HOSTS` | no | derived | Comma-separated `host` or `host:port`. **Replaces** the derived list entirely. `*` disables `Host` validation and logs a `WARN`; it is only accepted as the sole value. A port-less entry matches any port. |
| `REDMINE_MCP_ALLOWED_ORIGINS` | no | `[]` (Origin validation off) | Comma-separated absolute origins, each with a scheme (`https://app.example.com`). `*` and `null` are rejected. When non-empty this also enables an exact-match CORS layer; when empty no CORS headers are sent at all. |

Both allowlists reject a value that is set but contains no usable entries
(`" , "`): "set, but empty" must not silently become "unset", because an empty
`Host` allowlist means *allow every host*.
| `REDMINE_MCP_MAX_REQUEST_BODY_BYTES` | no | `4194304` (4 MiB) | 1 KiB – 64 MiB. Enforced while streaming the body, so `Content-Length` cannot be lied about; oversized payloads get `413`. |
| `HEALTH_INTROSPECTION_TTL_SECONDS` | no | `30` | 0–3600. How long a `/readyz` Redmine probe stays cached. `0` disables caching. |

The derived `Host` allowlist is always `localhost`, `127.0.0.1`, `::1`, plus
the `PUBLIC_HOST` entry when set. It is logged at `INFO` on startup, so a `403`
can be diagnosed from the boot line.

`Host` validation applies to the MCP route only — the health endpoints answer
regardless of the `Host` header. They carry no configuration, so the exposure
is the single bit of "is Redmine reachable", but a rebound browser page can
read it.

`ConfigError::Invalid` never echoes a secret value — it describes the
expected shape instead (e.g. "must be a valid http(s) URL without userinfo",
not the rejected URL string when it might contain credentials pasted in by
mistake).

## Not yet implemented

These are read by the upstream reference server but not by
`ruprogress-mcp` yet; setting them today has no effect.

`REDMINE_USERNAME`, `REDMINE_PASSWORD`, `REDMINE_PUBLIC_URL`,
`REDMINE_SSL_CERT`, `REDMINE_SSL_CLIENT_CERT`,
`REDMINE_INTROSPECT_CLIENT_ID`, `REDMINE_INTROSPECT_CLIENT_SECRET`(`_FILE`),
`REDMINE_OAUTH_SCOPE_ENFORCEMENT`, `REDMINE_OAUTH_DISCOVERY_AS`,
`REDMINE_MCP_SCOPES`, `REDMINE_MCP_JWT_SIGNING_KEY`(`_FILE`), `FASTMCP_HOME`,
`REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS`, `REDMINE_OAUTH_CLIENT_ID`,
`REDMINE_OAUTH_CLIENT_SECRET`(`_FILE`), `ATTACHMENTS_DIR`,
`ATTACHMENT_MAX_DOWNLOAD_BYTES`, `AUTO_CLEANUP_ENABLED`,
`CLEANUP_INTERVAL_MINUTES`, `ATTACHMENT_EXPIRES_MINUTES`,
`REDMINE_MCP_UPLOAD_FILE_ROOTS`, `REDMINE_MCP_EXPOSE_ADMIN_TOOLS`,
`REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS`,
`REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS`.

## Exposing the server on a network

**The default bind is loopback-only** (`127.0.0.1:8000`). Nothing outside the
machine can reach it until you change `SERVER_HOST`.

**`SERVER_HOST=0.0.0.0` on its own is a startup error.** The exact message:

```
PUBLIC_HOST is required because SERVER_HOST is not a loopback address, so the
Host allowlist cannot be derived. Set PUBLIC_HOST to the hostname clients use
to reach this server, or set REDMINE_MCP_ALLOWED_HOSTS explicitly ("*" disables
Host validation entirely — only do this when a reverse proxy already validates
Host).
```

Why it is an error and not a warning: the `Host` allowlist is the only control
that detects DNS rebinding. Rebinding works by making the request *same-origin*
from the browser's point of view — `evil.com` resolves to your server's address
and a page on `evil.com` fetches `evil.com/mcp` — so there is no preflight and
CORS never runs. The one remaining signal is that the request arrives with
`Host: evil.com`. And in rmcp an *empty* allowlist means **allow every host**,
not "validation off", so a silently underivable list is fail-open. A boot-time
error beats a runtime `403` whose body explains nothing, after a startup warning
that scrolled away hours ago.

Three valid configurations:

```sh
# 1. Clients reach you by hostname (most deployments).
SERVER_HOST=0.0.0.0
PUBLIC_HOST=mcp.example.com          # allowlist gains mcp.example.com[:8000]

# 2. Several names, or a name that differs from PUBLIC_HOST's derivation.
SERVER_HOST=0.0.0.0
REDMINE_MCP_ALLOWED_HOSTS=mcp.example.com,mcp.internal:8000

# 3. A reverse proxy in front already validates Host. Logs a WARN.
SERVER_HOST=0.0.0.0
REDMINE_MCP_ALLOWED_HOSTS=*
```

A container published on localhost:

```sh
docker run --rm -p 8000:8000 \
  -e REDMINE_URL=https://redmine.example.com \
  -e REDMINE_API_KEY=... \
  -e SERVER_HOST=0.0.0.0 \
  -e PUBLIC_HOST=localhost \
  ruprogress-mcp --transport http
```

```yaml
services:
  ruprogress-mcp:
    image: ruprogress-mcp
    command: ["--transport", "http"]
    ports: ["8000:8000"]
    environment:
      REDMINE_URL: https://redmine.example.com
      REDMINE_API_KEY: ${REDMINE_API_KEY}
      SERVER_HOST: 0.0.0.0
      PUBLIC_HOST: localhost
```

Note that in the default `legacy` auth mode a non-loopback bind also logs a
`WARN`: there is one shared Redmine API key, so everyone who can reach the port
acts as that Redmine account. Put an authenticating proxy in front, or wait for
per-user auth.

## Health endpoints

Served on the HTTP transport only.

| Path | Checks | Codes |
|---|---|---|
| `/livez` | The process, and nothing else. | Always `200` |
| `/readyz` | A TTL-cached `GET /my/account.json` against Redmine. | `200` ready, `503` Redmine down |
| `/health` | Alias for `/readyz`, for parity with the reference server. | as `/readyz` |

`/livez` deliberately never touches Redmine: wired to a Kubernetes
`livenessProbe`, a dependency check would turn a Redmine blip into a restart
storm. Concurrent `/readyz` requests collapse into a single upstream probe —
setting `HEALTH_INTROSPECTION_TTL_SECONDS=0` turns that off along with the
cache, so concurrent probes each hit Redmine.

The body is readiness facts only:

```json
{ "status": "ready", "redmine": "up", "checked_at": "2026-08-06T12:00:00.123456+00:00" }
```

`checked_at` is when the probe actually ran, which on a cache hit is up to
`HEALTH_INTROSPECTION_TTL_SECONDS` ago. `redmine` is `up`, `down`, or `not_probed`. `not_probed` (with a `200`) is
returned in `legacy-per-user` and `oauth` modes, where the server owns no
credential to probe with; reporting "down" there would take the instance out of
rotation permanently. All three endpoints send `Cache-Control: no-store` and are
excluded from request tracing.

## `--print-config`

`ruprogress-mcp --print-config` resolves the config and prints
`Config::redacted_summary()` as JSON to stdout, then exits 0 without starting
a server. It includes the Redmine host, the transport (and its bind address and
MCP path), and the auth mode for operator debugging, but never a credential.

### The three redaction surfaces

They differ on purpose, because they have different audiences. Do not unify
them.

| Surface | Audience | Redmine host | Bind address | Config | Secrets |
|---|---|---|---|---|---|
| `Config::redacted_summary()` (`--print-config`) | An operator, locally | yes | yes | yes | never |
| `get_mcp_server_info` (MCP tool) | A language model | **no** | **no** | yes | never |
| `/readyz` | Anyone who can reach the port | **no** | **no** | **no** | never |

`get_mcp_server_info` omits internal topology because a model that has been
prompt-injected will happily relay it. `/readyz` is unauthenticated, so it
answers only the question it exists to answer.

## Divergences from the reference server

Three deliberate differences from `jztan/redmine-mcp-server`. A silent
behaviour difference is worse than a documented one.

1. **The default bind is `127.0.0.1:8000`, not `0.0.0.0:8000`.** An MCP server
   whose only auth is one shared API key, reachable on every interface, is one
   `docker run -p` away from being an unauthenticated Redmine proxy.
2. **`/health` reports readiness only** — no `version`, `auth_mode`,
   `read_only`, or `plugin_flags`, which the reference server includes. Those
   answer no readiness question and are configuration disclosure on an
   unauthenticated endpoint. They remain available via `get_mcp_server_info`
   and `--print-config`.
3. **A non-loopback `SERVER_HOST` without `PUBLIC_HOST` or
   `REDMINE_MCP_ALLOWED_HOSTS` refuses to start.** Porting an upstream `.env`
   that sets only `SERVER_HOST=0.0.0.0` requires adding one variable. See
   "Exposing the server on a network".

# Configuration

Only the variables `ruprogress-mcp` currently reads are validated; the rest
are recorded here for later and are currently ignored.

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
| `REDMINE_PER_USER_TRUST_PROXY` | yes, in `legacy-per-user` mode | — | Must be exactly `true`. Absent/false refuses to start (`Missing`) — this is the operator's explicit attestation that a TLS-terminating proxy sits in front and does not forward client `X-Forwarded-Proto`. `legacy-per-user` is additionally rejected outright on the `stdio` transport (`Conflict`): there is no per-request header to carry a credential over stdio. Startup logs a `WARN` naming this assumption every time the mode is enabled. See `docs/legacy-per-user-auth.md`. |
| `REDMINE_PER_USER_AUDIT_IDENTITY` | no | `false` | In `legacy-per-user` mode, logs one line per tool call naming a per-process, non-reversible fingerprint of the inbound `X-Redmine-API-Key` (never the key or a resolved Redmine identity) — see `docs/legacy-per-user-auth.md`. |
| `REDMINE_MCP_BASE_URL` | yes, in `oauth` mode | — | This server's own public base URL, embedded in the `WWW-Authenticate` challenge and OAuth discovery documents. Must be absolute `http`/`https` with no userinfo, query, or fragment. `http` on a non-loopback host logs a `WARN` rather than erroring. |
| `REDMINE_INTROSPECT_CLIENT_ID` | yes, in `oauth` mode | — | The confidential OAuth client id used to authenticate RFC 7662 introspection requests to Redmine's Doorkeeper. |
| `REDMINE_INTROSPECT_CLIENT_SECRET` | yes, in `oauth` mode | — | Mutually exclusive with `REDMINE_INTROSPECT_CLIENT_SECRET_FILE`. |
| `REDMINE_INTROSPECT_CLIENT_SECRET_FILE` | yes, in `oauth` mode (alternative) | — | Path to a file containing the secret (Docker/K8s secret mount). Setting both this and `REDMINE_INTROSPECT_CLIENT_SECRET` is a `Conflict`. |
| `REDMINE_OAUTH_TOKEN_CACHE_TTL_SECONDS` | no | `60` | 0–3600. How long a positive introspection result is cached, further capped by the token's own `exp`. `0` disables caching entirely. See `docs/oauth-setup.md`. |
| `REDMINE_OAUTH_DISCOVERY_AS` | no, `oauth` mode only | `redmine` | `redmine` or `self`. `self` serves the RFC 8414 authorization-server document at the root well-known path with `issuer = REDMINE_MCP_BASE_URL` (and 404s the suffixed path) instead of the default — see `docs/oauth-setup.md`. |
| `REDMINE_MCP_SCOPES` | no, `oauth` mode only | the full advertised set | Whitespace-separated subset of the scopes this server advertises in its OAuth discovery documents. Every entry must already be advertised in the current mode (respecting `REDMINE_MCP_READ_ONLY`/agile/tags gating); an out-of-set entry refuses to boot, listing the accepted set. Narrows advertisement; enforcement is `REDMINE_OAUTH_SCOPE_ENFORCEMENT` below. |
| `REDMINE_OAUTH_SCOPE_ENFORCEMENT` | no, `oauth` mode only | `on` | `on` or `off`. `off` disables both `tools/list` filtering and `tools/call` scope denial, restoring unfiltered behaviour, and logs a startup `WARN` — intended only for tokens minted before the OAuth application advertised scopes. See `docs/oauth-setup.md`. |
| `REDMINE_SSL_VERIFY` | no | `true` | `false` is accepted but logs a `WARN` — never silently downgrades without a trace. |
| `REDMINE_MCP_READ_ONLY` | no | `false` | Removes every tool in `readonly::write_tools::ALL` from the router (hides from `tools/list` **and** rejects `tools/call`). |
| `REDMINE_MCP_SCHEMA_DIALECT` | no | `strict` | One of `strict`, `portable`. `portable` inlines every `inputSchema`'s `$ref`/`$defs` and collapses `{"type":["T","null"]}` to `{"type":"T"}`, for clients (Google Vertex/Gemini) whose function-calling schema validator rejects the rich JSON Schema 2020-12 form. `outputSchema` is unaffected either way — see ADR 0007. |
| `REDMINE_AGILE_ENABLED` | no | `false` | `plugin_flags.agile` (`get_mcp_server_info`). Adds no new tools — `true` makes `get_redmine_issue` report `story_points`/`agile_sprint_id`/`agile_position` (RedmineUP Agile plugin) and lets `update_redmine_issue` change them; `false` (the default) makes the fields absent from `get_redmine_issue` and any of the three parameters on `update_redmine_issue` fail with `MISCONFIGURED` before any write happens. |
| `REDMINE_CHECKLISTS_ENABLED` | no | `false` | `plugin_flags.checklists`. `true` registers `get_checklist`/`create_checklist_item`/`update_checklist_item` (RedmineUP Checklists Pro plugin); `false` (the default) de-registers them from the router entirely — they are absent from `tools/list` and `tools/call` fails with "tool not found", not an in-band error. |
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
| `PUBLIC_SCHEME` | no | `https` if `PUBLIC_PORT` is `443`, else `http` | `http` or `https`. Feeds the origin used to build `/files/{uuid}` attachment URLs (`public_base`); has no effect on the `Host` allowlist. |

The derived `Host` allowlist is always `localhost`, `127.0.0.1`, `::1`, plus
the `PUBLIC_HOST` entry when set. It is logged at `INFO` on startup, so a `403`
can be diagnosed from the boot line.

`Host` validation applies to the MCP route only — the health endpoints answer
regardless of the `Host` header. They carry no configuration, so the exposure
is the single bit of "is Redmine reachable", but a rebound browser page can
read it. `/files/{uuid}` (below) gets its own copy of the same `Host` check,
reusing the same allowlist.

### `public_base`: the origin used for `/files/{uuid}` links

Building an attachment download URL needs an origin, not just a `Host`
allowlist. It is derived once at startup, independently of
`REDMINE_MCP_ALLOWED_HOSTS`:

- With `PUBLIC_HOST` set: `{PUBLIC_SCHEME}://{PUBLIC_HOST}[:{PUBLIC_PORT}]`.
- Without it: only a **loopback** `SERVER_HOST` can derive one
  (`http://<bind-ip>:<port>`, correct for a client on the same machine).

A non-loopback `SERVER_HOST` with no `PUBLIC_HOST` is a startup error here
even when `REDMINE_MCP_ALLOWED_HOSTS=*` was set — that variable disables the
`Host` *check*, but building a working `/files/{uuid}` URL still needs a real
origin a client could reach.

`ConfigError::Invalid` never echoes a secret value — it describes the
expected shape instead (e.g. "must be a valid http(s) URL without userinfo",
not the rejected URL string when it might contain credentials pasted in by
mistake).

## Not yet implemented

These are recognised names that `ruprogress-mcp` does not read yet; setting
them today has no effect.

`REDMINE_USERNAME`, `REDMINE_PASSWORD`,
`REDMINE_SSL_CERT`, `REDMINE_SSL_CLIENT_CERT`,
`REDMINE_MCP_JWT_SIGNING_KEY`(`_FILE`), `FASTMCP_HOME`,
`REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS`, `REDMINE_OAUTH_CLIENT_ID`,
`REDMINE_OAUTH_CLIENT_SECRET`(`_FILE`),
`REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS`,
`REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS`.

The 8 attachment-related variables are validated and read (see "Attachment
store" below). `get_redmine_attachment`
reads `ATTACHMENTS_DIR`, `ATTACHMENT_MAX_DOWNLOAD_BYTES`,
`ATTACHMENT_STORE_MAX_BYTES`, `AUTO_CLEANUP_ENABLED`,
`CLEANUP_INTERVAL_MINUTES`, and `ATTACHMENT_EXPIRES_MINUTES`;
`upload_file`/`cleanup_attachment_files` read
`REDMINE_MCP_UPLOAD_FILE_ROOTS` and `REDMINE_MCP_EXPOSE_ADMIN_TOOLS`
respectively; `REDMINE_PUBLIC_URL` rewrites every `content_url`
this server emits.

## Attachment store

Local, on-disk staging for downloaded Redmine attachments. The store always
exists (there is no way to disable it) and the directory is created at
startup. `get_redmine_attachment` is the first consumer: it
streams a Redmine attachment's bytes into the store, enforcing the per-file
and whole-store caps below against bytes actually received rather than any
header or metadata field, then returns a `/files/{uuid}` URL (HTTP
transport) or an absolute `file_path` (stdio transport).

| Variable | Required | Default | Notes |
|---|---|---|---|
| `ATTACHMENTS_DIR` | no | `{temp_dir}/ruprogress-mcp-attachments` | Created `0700` on Unix at startup; a `WARN` is logged on other platforms, where permissions rely on inherited ACLs. Per-installation, not per-process, so a restarted process can still reap a predecessor's files. |
| `ATTACHMENT_MAX_DOWNLOAD_BYTES` | no | `209715200` (200 MiB) | Positive integer. The per-file cap. |
| `ATTACHMENT_STORE_MAX_BYTES` | no | `2147483648` (2 GiB) | Positive integer. The whole-store cap. Must be `>=` `ATTACHMENT_MAX_DOWNLOAD_BYTES`, or a startup `Conflict` — a smaller store cap would mean no single download could ever fit. |
| `AUTO_CLEANUP_ENABLED` | no | `true` | Whether the background sweeper task runs at all. |
| `CLEANUP_INTERVAL_MINUTES` | no | `15` | Positive integer. How often the sweeper runs. |
| `ATTACHMENT_EXPIRES_MINUTES` | no | `60` | Positive integer. How long a stored file stays fetchable. Checked on every lookup (not just by the interval sweeper), so an expired file is refused immediately rather than up to `CLEANUP_INTERVAL_MINUTES` late. |
| `REDMINE_MCP_UPLOAD_FILE_ROOTS` | no | `[]` | Comma-separated **absolute** directory paths `upload_file`'s `file_path` source may read from, in addition to `ATTACHMENTS_DIR` itself (always allowed). Empty means only `ATTACHMENTS_DIR` is allowed. |
| `REDMINE_MCP_EXPOSE_ADMIN_TOOLS` | no | `false` | Registers `cleanup_attachment_files` when `true`; otherwise the tool does not appear in `tools/list` at all. |
| `REDMINE_PUBLIC_URL` | no | — | Must be a valid `http`/`https` URL. Rewrites every `content_url` this server emits (`get_redmine_issue`'s/`create_redmine_issue`'s/`update_redmine_issue`'s `attachments[*]`, `list_files`/`upload_file`, wiki page attachments) whose scheme+host+port matches `REDMINE_URL`'s, preserving path, query, fragment, and any reverse-proxy sub-path baked into `REDMINE_PUBLIC_URL` itself. A `content_url` whose origin does not match is left untouched. |

`GET /files/{uuid}` (HTTP transport only) serves a stored file:
`Content-Disposition: attachment`, `X-Content-Type-Options: nosniff`,
`Cache-Control: no-store`, and a sanitised filename. `404` for an unknown or
expired UUID; `403` for a `Host` header outside the same allowlist `/mcp`
uses (see "`public_base`" above).

`upload_file`'s two sources have two different, unrelated size ceilings:
`content_base64` is bounded by `REDMINE_MCP_MAX_REQUEST_BODY_BYTES` on the
HTTP transport (the request body itself; the base64 encoding means roughly
three-quarters of that byte count survives decoding); `file_path` has its own
fixed 50 MiB limit, independent of `ATTACHMENT_MAX_DOWNLOAD_BYTES` (which
bounds the opposite direction, a Redmine attachment downloading onto local
disk). A `file_path` upload is refused with `PATH_NOT_ALLOWED` — never a path
or existence detail — unless it resolves inside `ATTACHMENTS_DIR` or a
`REDMINE_MCP_UPLOAD_FILE_ROOTS` entry.

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
acts as that Redmine account. Put an authenticating proxy in front, or use
`REDMINE_AUTH_MODE=legacy-per-user` or `oauth` so each caller presents their
own credential.

## Health endpoints

Served on the HTTP transport only.

| Path | Checks | Codes |
|---|---|---|
| `/livez` | The process, and nothing else. | Always `200` |
| `/readyz` | A TTL-cached probe: `GET /my/account.json` in `legacy` mode, RFC 7662 introspection with a synthetic token in `oauth` mode, none in `legacy-per-user`. | `200` ready, `503` not ready |
| `/health` | Alias for `/readyz`. | as `/readyz` |

`/livez` deliberately never touches Redmine: wired to a Kubernetes
`livenessProbe`, a dependency check would turn a Redmine blip into a restart
storm. Concurrent `/readyz` requests collapse into a single upstream probe —
setting `HEALTH_INTROSPECTION_TTL_SECONDS=0` turns that off along with the
cache, so concurrent probes each hit Redmine.

The body is readiness facts only, `legacy`/`legacy-per-user`:

```json
{ "status": "ready", "redmine": "up", "checked_at": "2026-08-06T12:00:00.123456+00:00" }
```

`checked_at` is when the probe actually ran, which on a cache hit is up to
`HEALTH_INTROSPECTION_TTL_SECONDS` ago. `redmine` is `up`, `down`, or
`not_probed`. `not_probed` (with a `200`) is returned in `legacy-per-user`
mode, where the server owns no credential to probe with; reporting "down"
there would take the instance out of rotation permanently.

`oauth` mode probes introspection instead (bypassing the token cache — the
probe uses a synthetic token no real Doorkeeper token matches) and gains a
`checks` field:

```json
{ "status": "ready", "redmine": "ok", "checks": { "introspection": "ok" }, "checked_at": "..." }
```

`redmine`/`checks.introspection` (identical) is `ok` (`200 {"active": false}`
— the inactive result is expected, what matters is that introspection
answered), `misconfigured` (Doorkeeper rejected this server's own client
credentials — `401`/`403`/`404`, `503` overall), or `unreachable` (transport
error, `5xx`, or timeout — `503` overall).

All three endpoints send `Cache-Control: no-store` and are excluded from
request tracing.

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

## Deliberately conservative defaults

Four choices that trade convenience for a safer default. A silent behaviour
difference is worse than a documented one.

1. **The default bind is `127.0.0.1:8000`, not `0.0.0.0:8000`.** An MCP server
   whose only auth is one shared API key, reachable on every interface, is one
   `docker run -p` away from being an unauthenticated Redmine proxy.
2. **`/health` reports readiness only** — no `version`, `auth_mode`,
   `read_only`, or `plugin_flags`. Those answer no readiness question and are
   configuration disclosure on an unauthenticated endpoint. They remain
   available via `get_mcp_server_info` and `--print-config`.
3. **A non-loopback `SERVER_HOST` without `PUBLIC_HOST` or
   `REDMINE_MCP_ALLOWED_HOSTS` refuses to start.** See "Exposing the server on
   a network".
4. **`PUBLIC_HOST` is required for a non-loopback `SERVER_HOST` even when
   `REDMINE_MCP_ALLOWED_HOSTS=*` is set.** That variable turns off the `Host`
   *check*, but `/files/{uuid}` URLs still need a real origin to build from —
   an unreachable `http://0.0.0.0:8000/files/...` link is worse than a
   startup error naming the fix.

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
| `REDMINE_AGILE_ENABLED` | no | `false` | Surfaced in `plugin_flags.agile` (`get_mcp_server_info`). No agile tools exist yet. |
| `REDMINE_CHECKLISTS_ENABLED` | no | `false` | `plugin_flags.checklists`. |
| `REDMINE_PRODUCTS_ENABLED` | no | `false` | `plugin_flags.products`. |
| `REDMINE_CRM_ENABLED` | no | `false` | `plugin_flags.crm`. |
| `REDMINE_DMSF_ENABLED` | no | `false` | `plugin_flags.dmsf`. |
| `REDMINE_TAGS_ENABLED` | no | `false` | `plugin_flags.tags`. |

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
`REDMINE_OAUTH_CLIENT_SECRET`(`_FILE`), `HEALTH_INTROSPECTION_TTL_SECONDS`,
`SERVER_HOST`, `SERVER_PORT`, `PUBLIC_HOST`, `PUBLIC_PORT`, `ATTACHMENTS_DIR`,
`ATTACHMENT_MAX_DOWNLOAD_BYTES`, `AUTO_CLEANUP_ENABLED`,
`CLEANUP_INTERVAL_MINUTES`, `ATTACHMENT_EXPIRES_MINUTES`,
`REDMINE_MCP_UPLOAD_FILE_ROOTS`, `REDMINE_MCP_EXPOSE_ADMIN_TOOLS`,
`REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS`,
`REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS`.

## `--print-config`

`ruprogress-mcp --print-config` resolves the config and prints
`Config::redacted_summary()` as JSON to stdout, then exits 0 without starting
a server. It includes the Redmine host and auth mode for operator debugging,
but never a credential. This is deliberately a *different* redaction surface
than the `get_mcp_server_info` MCP tool, which additionally omits the host —
see `crates/ruprogress-mcp/src/tools/meta.rs`.

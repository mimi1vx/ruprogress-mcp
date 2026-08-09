# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `REDMINE_AUTH_MODE=oauth` now works end to end for bearer-token
  authentication: an axum middleware guards the whole `/mcp` route (including
  `initialize`), extracting the inbound `Authorization: Bearer` token and
  validating it by RFC 7662 introspection against Redmine's Doorkeeper,
  cached by a SHA-256 digest of the token (never the token itself) with a TTL
  capped by the token's own `exp`. A missing/malformed/invalid token gets a
  `401` carrying `WWW-Authenticate: Bearer resource_metadata="..."`; a broken
  or misconfigured introspection endpoint gets a `503` with `Retry-After`,
  never a `401`. The validated token is forwarded to Redmine verbatim.
  Requires the new `REDMINE_INTROSPECT_CLIENT_ID`/
  `REDMINE_INTROSPECT_CLIENT_SECRET`(`_FILE`) variables; `oauth` on the stdio
  transport is now a startup error, matching `legacy-per-user`. Scope
  enforcement, discovery documents, and `/revoke` are not implemented yet —
  see `docs/oauth-setup.md`.
- `REDMINE_AUTH_MODE=legacy-per-user` is now implemented: each HTTP request
  carries its own Redmine credential in `X-Redmine-API-Key` instead of the
  server holding one shared key. No ambient fallback and no cross-request
  reuse — a missing, empty, malformed, oversized, or duplicated header is
  rejected before any Redmine request is attempted, while `initialize`/
  `tools/list` still succeed with no header. `REDMINE_PER_USER_AUDIT_IDENTITY`
  logs a non-reversible per-process fingerprint of the caller's key, never the
  key itself. See `docs/legacy-per-user-auth.md` for the threat model.
- `create_redmine_issue`/`update_redmine_issue` accept an optional `uploads`
  array (max 10 items) to attach files in the same request, via the same
  `content_base64`/`file_path` sources as `upload_file`
  (`source_url` always refused with `UNSUPPORTED_SOURCE`). Files are attached
  through Redmine's issue-native `uploads: [{token, ...}]` shape, not the
  Files-module two-step flow; the response's `issue.attachments` reflects the
  newly attached files.
- `REDMINE_PUBLIC_URL` now rewrites every `content_url` this server emits
  (`get_redmine_issue`'s/`create_redmine_issue`'s/`update_redmine_issue`'s
  `attachments[*]`, `list_files`/`upload_file`, wiki page attachments) whose
  origin matches `REDMINE_URL`'s, preserving path, query, fragment, and any
  reverse-proxy sub-path baked into `REDMINE_PUBLIC_URL` itself.
- `upload_file`: uploads a file and attaches it to a project's Files module
  (`POST /uploads.json` then `POST /projects/{id}/files.json`). Accepts
  `content_base64` (requires `filename`) or `file_path` (an absolute path
  inside `ATTACHMENTS_DIR` or a `REDMINE_MCP_UPLOAD_FILE_ROOTS` entry, capped
  at 50 MiB and validated against symlink/FIFO/device traversal before it is
  ever opened); `source_url` is recognised but always refused with
  `UNSUPPORTED_SOURCE`. Write tool, blocked in read-only mode.
- `cleanup_attachment_files`: runs the local attachment store's expiry sweep
  on demand and reports `{cleaned_files, cleaned_bytes, cleaned_mb}`. Mutates
  only local disk, never Redmine, so it still works in read-only mode;
  registered only when `REDMINE_MCP_EXPOSE_ADMIN_TOOLS=true`.
- `REDMINE_MCP_UPLOAD_FILE_ROOTS` and `REDMINE_MCP_EXPOSE_ADMIN_TOOLS` are now
  read by `upload_file`/`cleanup_attachment_files` respectively (previously
  validated but unread).
- `list_files`: lists a project's Files-module entries (`GET
  /projects/{id}/files.json`) — not issue attachments, not DMSF.
- `delete_file`: deletes an attachment by id (`DELETE /attachments/{id}.json`).
  Redmine's endpoint deletes any attachment this credential can reach, not
  just project Files, so the tool always requires
  `confirm_delete_any_attachment=true`; write tool, blocked in read-only
  mode.
- `get_redmine_attachment`: downloads a Redmine attachment by id and stages
  it in the local attachment store, returning a `/files/{uuid}` URL over
  HTTP or an absolute `file_path` over stdio (`uri_type` tells you which).
  The per-file byte cap (`ATTACHMENT_MAX_DOWNLOAD_BYTES`) is enforced against
  bytes actually streamed, never a `Content-Length` header or Redmine's own
  `filesize` metadata; a full store first sweeps expired entries, then
  refuses with `STORE_FULL` rather than filling the disk.
- A local attachment store (now used by `get_redmine_attachment`):
  `AttachmentStore` stages downloaded Redmine attachments under
  `ATTACHMENTS_DIR` in per-UUID
  directories with a sanitised basename, enforces a per-file
  (`ATTACHMENT_MAX_DOWNLOAD_BYTES`) and whole-store
  (`ATTACHMENT_STORE_MAX_BYTES`) byte cap, and expires entries after
  `ATTACHMENT_EXPIRES_MINUTES` both lazily (on lookup) and via a background
  sweeper (`AUTO_CLEANUP_ENABLED`, `CLEANUP_INTERVAL_MINUTES`) that also
  reclaims a predecessor process's orphaned files after a restart.
- `GET /files/{uuid}` (HTTP transport only): serves a stored file with
  `Content-Disposition: attachment`, `X-Content-Type-Options: nosniff`,
  `Cache-Control: no-store`, and the same `Host` allowlist check as `/mcp`.
- `PUBLIC_SCHEME` and a derived `public_base` origin, for building
  `/files/{uuid}` URLs correctly behind a TLS-terminating proxy.
- `REDMINE_MCP_UPLOAD_FILE_ROOTS`, `REDMINE_MCP_EXPOSE_ADMIN_TOOLS`, and
  `REDMINE_PUBLIC_URL` are now validated (previously listed as "not yet
  implemented"); `REDMINE_PUBLIC_URL`'s `content_url`-rewriting behaviour is
  not applied yet.
- Opt-in `REDMINE_MCP_SCHEMA_DIALECT=portable` for clients whose provider
  rejects `$ref`/`$defs` and nullable type arrays in function-calling
  declarations (Google Vertex/Gemini). Default `strict` is unchanged.
- Streamable HTTP transport (`--transport http`), serving the same MCP server
  as stdio at `FASTMCP_STREAMABLE_HTTP_PATH` (default `/mcp`). Stateless mode:
  `GET`/`DELETE` on the MCP route return `405`, and no session id is issued.
- `Host` and `Origin` allowlisting, a streamed request-body cap, exact-match
  CORS, `X-Content-Type-Options: nosniff`, and graceful drain on `SIGTERM`.
- `/livez`, `/readyz`, and a `/health` alias, with a TTL-cached Redmine probe
  that collapses concurrent requests into one upstream call.
- HTTP configuration: `SERVER_HOST`, `SERVER_PORT`, `PUBLIC_HOST`,
  `PUBLIC_PORT`, `FASTMCP_STREAMABLE_HTTP_PATH`, `REDMINE_MCP_ALLOWED_HOSTS`,
  `REDMINE_MCP_ALLOWED_ORIGINS`, `REDMINE_MCP_MAX_REQUEST_BODY_BYTES`,
  `HEALTH_INTROSPECTION_TTL_SECONDS`.
- `get_mcp_server_info` reports the active `transport`.
- Cargo workspace scaffold with `redmine-client` and `ruprogress-mcp` crates.
- Workspace-wide lints, `rustfmt.toml`, `deny.toml`, and CI gates.
- Every tool returns structured content (`structuredContent`) validating
  against a declared JSON output schema, plus `readOnlyHint`/
  `idempotentHint`/`openWorldHint` annotations.
- Redmine API failures are reported in-band as `{error, code, retryable,
  hint}` results (`isError: true`) instead of protocol-level errors, so a
  model can see and react to them.
- `REDMINE_MCP_MAX_RESPONSE_ITEMS` (default 200) and
  `REDMINE_MCP_MAX_RESPONSE_BYTES` (default 256 KiB) cap list-tool response
  size, surfaced as `pagination.truncated` plus a hint rather than a silent
  cut.
- `redmine-client`: `Scoped::get_collection`/`Scoped::fetch_page` request
  primitives for Redmine's un-paginated and single-page-only endpoints.
- Six discovery/enumeration tools: `list_redmine_trackers`,
  `list_project_trackers`, `list_redmine_issue_statuses`,
  `list_redmine_issue_priorities`, `list_redmine_users`,
  `list_redmine_queries`, alongside the existing `get_current_user`. Each
  resolves a name to an id before a create/update tool needs it.
  `list_redmine_users` requires an admin credential and clamps `limit` to
  1-100; a non-admin call returns a `FORBIDDEN` error naming
  `get_current_user` as the next step instead of retrying.
- `redmine-client`: `Tracker` and `IssueStatus` models, an `admin` field on
  `User`, a `trackers` field on `Project` (populated only when
  `include=trackers` was requested), and `Scoped::list_trackers`/
  `list_issue_statuses`/`list_issue_priorities`/`list_users`/
  `list_saved_queries`.

### Fixed

- Tool schemas no longer advertise the non-standard `uint32`/`uint64`
  integer `format` values `schemars` emits for `u32`/`u64` fields, which made
  strict JSON Schema clients (e.g. opencode's Ajv-based validator) log an
  "unknown format" warning per field on every `tools/list`.

### Changed

- The default HTTP bind is `127.0.0.1:8000`, not the reference server's
  `0.0.0.0:8000`.
- `list_redmine_projects` now returns `{"projects": [...], "pagination":
  {...}}` instead of a bare JSON array, so `structuredContent` is a JSON
  object per the MCP spec.
- The prompt-injection delimiter scheme is now explained once per session in
  the MCP `initialize` instructions, instead of a repeated text block on
  every tool response.
- `/health` reports readiness only; it no longer carries version, auth mode,
  read-only state, or plugin flags. Those remain on `get_mcp_server_info` and
  `--print-config`.
- A non-loopback `SERVER_HOST` with neither `PUBLIC_HOST` nor
  `REDMINE_MCP_ALLOWED_HOSTS` now **fails at startup** rather than serving with
  `Host` validation silently disabled. Porting an upstream `.env` that sets
  only `SERVER_HOST=0.0.0.0` needs one added variable; the error message names
  both options and the `*` opt-out.

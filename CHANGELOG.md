# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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

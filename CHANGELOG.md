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

### Changed

- The default HTTP bind is `127.0.0.1:8000`, not the reference server's
  `0.0.0.0:8000`.
- `/health` reports readiness only; it no longer carries version, auth mode,
  read-only state, or plugin flags. Those remain on `get_mcp_server_info` and
  `--print-config`.
- A non-loopback `SERVER_HOST` with neither `PUBLIC_HOST` nor
  `REDMINE_MCP_ALLOWED_HOSTS` now **fails at startup** rather than serving with
  `Host` validation silently disabled. Porting an upstream `.env` that sets
  only `SERVER_HOST=0.0.0.0` needs one added variable; the error message names
  both options and the `*` opt-out.

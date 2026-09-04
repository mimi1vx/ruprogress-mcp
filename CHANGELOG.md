# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.10.1](https://github.com/mimi1vx/ruprogress-mcp/compare/v0.10.0...v0.10.1) - 2026-09-04

### Other

- update Cargo.toml dependencies

## [0.10.0](https://github.com/mimi1vx/ruprogress-mcp/compare/v0.9.3...v0.10.0) - 2026-09-04

### Added

- clear issue fields by name so portable-schema clients can too
- [**breaking**] seal IssueUpdate and reject a stray Unchanged at serialization
- [**breaking**] clear issue fields that could previously only be set

### Fixed

- close manage_subtasks bypass via clear_fields reparenting

## [0.9.3](https://github.com/mimi1vx/ruprogress-mcp/compare/v0.9.2...v0.9.3) - 2026-08-25

### Other

- add project logo and GitHub social preview header

## [0.9.2](https://github.com/mimi1vx/ruprogress-mcp/compare/v0.9.1...v0.9.2) - 2026-08-24

### Other

- fail the build when either README drifts from the code
- correct the Docker publish claim and expand both crate READMEs

## [0.9.1](https://github.com/mimi1vx/ruprogress-mcp/compare/v0.9.0...v0.9.1) - 2026-08-24

### Fixed

- *(deps)* keep tower-http at 0.6 and rename rand 0.10 OsRng/TryRngCore
- *(main)* exit immediately on SIGTERM instead of falling through runtime drop
- *(server)* drop the unused async from list_tools

### Other

- *(redmine-client)* make the chunked-abort limits test buffer-independent

## [0.9.0] - 2026-08-24

### Security

- `redmine-client` now refuses to send credentials anywhere but the
  configured Redmine origin. `download_attachment`'s `content_url` (an
  absolute URL Redmine itself returns) is checked against the client's
  configured origin — scheme, host, and port must all match, and embedded
  userinfo is rejected outright — before the request is built, closing a
  credential-exfiltration path a compromised Redmine, plugin, or reverse
  proxy could otherwise use. Separately, the shared `reqwest::Client` now
  carries a same-origin redirect policy: previously its default
  `redirect::Policy::limited(10)` would follow a cross-origin `3xx`
  response on *any* endpoint, and reqwest does not strip the
  `X-Redmine-API-Key` header (the default credential) on such a redirect —
  so a redirecting Redmine leaked the API key to whatever host it named. A
  Redmine that legitimately redirects to a different origin (e.g. `http`
  to `https`, or attachments served from a CDN) now fails with an in-band
  `UNEXPECTED_RESPONSE` instead of silently following the redirect; point
  `REDMINE_URL` at the final origin directly.
- `oauth-proxy`'s refresh-token redemption is now single-use atomically:
  previously a refresh token was only looked up, not consumed, until the
  handler finished, so two requests presenting the same token concurrently
  could both refresh upstream and each mint an independent valid pair,
  defeating rotation and reuse detection (RFC 9700 §4.14.2). Redemption now
  transitions the token under one lock acquisition; a second concurrent
  redemption is treated the same as replaying an already-rotated token —
  the session is revoked upstream and both requests fail. A dropped,
  cancelled, or early-returning refresh leaves the token immediately
  reusable rather than stuck. `UpstreamStore::replace` is now conditional
  too, so an in-flight refresh can no longer resurrect a session a
  concurrent `/revoke` already removed.
- The default `legacy` auth mode now refuses to start on a non-loopback
  `SERVER_HOST` unless `REDMINE_MCP_ALLOW_UNAUTHENTICATED_NETWORK=true` is
  set. A shared `REDMINE_API_KEY` authenticates this server to Redmine, not
  the caller to this server, so anyone who could reach a non-loopback bind
  acted as that Redmine account; this previously only logged a `WARN`,
  which the shipped `Dockerfile`/`docker-compose.yml` example configuration
  (`SERVER_HOST=0.0.0.0` plus a published port) would have hit silently.
  The `WARN` is unchanged and still fires once the variable is set.
  `docker-compose.yml` now also publishes `127.0.0.1:8000:8000` instead of
  `8000:8000`. **Breaking:** an existing `legacy` + non-loopback HTTP
  deployment will refuse to start until the variable is set, loopback is
  bound instead, or another auth mode is used — this speed bump is one env
  var away from being overridden, and does not by itself authenticate
  callers; `legacy-per-user`/`oauth`/`oauth-proxy` still exist for that.
- `redmine-client` now enforces `Limits::max_response_bytes` while streaming
  a response, instead of after buffering the whole body into memory. A
  response declaring an over-limit `Content-Length` is rejected before any
  body byte is read; a chunked response with no (or a lying) `Content-Length`
  is now read chunk-by-chunk and aborted mid-stream — dropping the
  connection — the moment the running total crosses the limit. Previously a
  malicious, compromised, or misconfigured upstream (or an intermediary
  error page returned in its place) could make the process allocate an
  unbounded `Bytes` buffer per in-flight request; this covers the
  status-error path, JSON decoding, and both OAuth token-exchange error
  paths. `download_attachment` remains the sole, documented exemption: it
  streams to its caller, which owns its own byte cap.
- `get_redmine_attachment` now reserves its declared `filesize` against
  `ATTACHMENT_STORE_MAX_BYTES` atomically, before streaming a single byte,
  instead of only checking committed entries. Previously the check and the
  accounting were separated by the whole download: N concurrent downloads
  all observed the same (unchanged) committed total, all passed admission,
  and could together write up to `N × ATTACHMENT_MAX_DOWNLOAD_BYTES` to
  disk — enough concurrent requests could exhaust local storage regardless
  of the configured cap. The reservation grows mid-stream if the actual
  byte count exceeds the declared size (still bounded by
  `ATTACHMENT_MAX_DOWNLOAD_BYTES`, so a stale `filesize` degrades to the
  previous behaviour rather than a hard failure) and is always released —
  on commit, abort, a mid-stream error, or a dropped/cancelled request —
  so a refused or failed download's quota is immediately reusable.
- `content_base64` uploads (`upload_file`, `manage_document`'s `create`
  action, and each `uploads[]` item on `create_redmine_issue`/
  `update_redmine_issue`) are now capped at 50 MiB decoded, the same limit
  `file_path` already enforced — previously only the HTTP transport's
  `REDMINE_MCP_MAX_REQUEST_BODY_BYTES` bounded the encoded input, and
  stdio had no cap at all, so a locally connected client could make the
  process allocate an unbounded decode buffer. The check runs against the
  base64 crate's own documented decode-length estimate before any decode
  allocation, then again on the exact decoded length, so peak allocation
  is bounded rather than proportional to the input. `create_redmine_issue`/
  `update_redmine_issue`'s `uploads[]` also gained a 100 MiB aggregate
  budget across all items (either source): previously the existing 10-item
  cap alone still let one call buffer up to 10 × 50 MiB ≈ 500 MiB of
  decoded bytes before the first attachment was uploaded. Both checks run
  before any `POST /uploads.json` request, so a rejected batch never
  strands an orphaned upload token on the Redmine server. **Breaking:** a
  caller relying on a `content_base64` upload over 50 MiB on stdio, or a
  `uploads[]` batch over 100 MiB aggregate, is now refused with an in-band
   `FILE_TOO_LARGE` instead of succeeding.

### Added

- `REDMINE_AUTH_MODE=oauth-proxy`: this server discovers as its own OAuth
  authorization server and accepts RFC 7591 Dynamic Client Registration
  (`POST /register`), for MCP clients that expect to register themselves
  rather than being hand-added to Redmine's admin panel. The RFC 8414
  authorization-server document is always served at the root well-known path
  (`issuer = REDMINE_MCP_BASE_URL`), naming this server's own
  `/authorize`/`/token`/`/register`/`/revoke`, `token_endpoint_auth_methods_supported:
  ["none"]` (every DCR client is public — no secret is ever issued or
  accepted), and `authorization_response_iss_parameter_supported: true`.
  `REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS` gates which redirect URIs a
  client may register (component-matched, never a raw-string glob — a
  pattern a glob would accept and this matcher rejects is a pattern nobody
  should have written), defaulting to loopback-only. The introspection
  credential, cache TTL, advertised scopes, and scope-enforcement flag are
  shared verbatim with `oauth` mode, so `tools/list` filtering and
  `INSUFFICIENT_SCOPE` denial apply identically once a caller holds a valid
  bearer token.
- `oauth-proxy` mode's authorization-code + PKCE flow: `GET /authorize`
  validates the client and redirect URI before anything else can redirect
  (an unregistered `client_id` or redirect URI is a plain `400` with no
  `Location` header — never an open redirect), then forwards the request to
  Redmine's own `/oauth/authorize` behind a second, independently generated
  PKCE pair this server holds on the client's behalf. `GET /auth/callback`
  exchanges the resulting Redmine code for an upstream access (and, when
  Doorkeeper issues one, refresh) token, and mints a short-lived
  authorization code of its own. `POST /token` redeems that code for an
  opaque `rup_at_`-prefixed proxy access token: a 256-bit CSPRNG handle,
  never a signed JWT and never the upstream Redmine token itself. Presenting
  the upstream token directly to `/mcp` is still `401`; presenting a valid
  proxy token resolves it to the stored upstream token and verifies that via
  the same introspection path `oauth` mode uses, so scope enforcement and
  `INSUFFICIENT_SCOPE` denial are unchanged. A replayed authorization code is
  `invalid_grant` and revokes the session it minted.
- `oauth-proxy` mode's `refresh_token` grant and mode-specific `POST
  /revoke`: a `rup_rt_`-prefixed proxy refresh token is minted alongside the
  access token whenever the upstream OAuth application issues one, and
  redeeming it at `/token` rotates both — a new pair is issued and the
  presented refresh token stops working. Replaying an already-rotated
  refresh token is treated as theft: the whole session is killed and
  revoked upstream immediately, not just the reused token. Upstream
  applications without `use_refresh_token` enabled still work access-only,
  with one process-wide `WARN` naming the missing setting. `POST /revoke`
  now has proxy-mode semantics: it accepts a `rup_at_`/`rup_rt_` token,
  deletes the pair and the upstream token set locally first (so revocation
  holds even if Redmine is unreachable), then revokes the upstream token,
  and always answers `200` per RFC 7009 — `oauth` mode's `/revoke` is
  unchanged. `get_mcp_server_info` reports `registered_clients` and
  `active_sessions` counts in this mode (`null` elsewhere) — no client
  identifiers, ever.
- Three tools for the RedmineUP Checklists Pro plugin, registered only when
  `REDMINE_CHECKLISTS_ENABLED=true`: `get_checklist`, `create_checklist_item`,
  `update_checklist_item`. With the flag off (the default) they are fully
  absent from `tools/list` and `tools/call` fails with "tool not found"
  rather than an in-band error — the first user of a new plugin-gating
  mechanism (`server.rs`'s `PLUGIN_TOOLS` route-removal table) that later
  plugin families reuse. The plugin's wire shapes are synthetic, derived
  from the reference implementation's handling of the plugin rather than a
  live capture (Checklists Pro is commercial) — see
  `crates/redmine-client/tests/fixtures/README.md`.
- `REDMINE_AGILE_ENABLED=true` makes `get_redmine_issue` report
  `story_points`/`agile_sprint_id`/`agile_position` from the RedmineUP Agile
  plugin, and lets `update_redmine_issue` change them — the three fields
  ride on both existing tools rather than adding a new one. Writes are a
  read-modify-write against `GET /issues/{id}/agile_data.json` because the
  plugin's nested `agile_data_attributes` replaces the whole row rather than
  merging it: an update naming only one field would otherwise null the
  others. `story_points: null` clears it; `agile_sprint_id: 0` removes the
  issue from its sprint (the plugin's own sentinel). With the flag off (the
  default), the three fields never appear and any of the three parameters on
  `update_redmine_issue` fails in-band with `MISCONFIGURED` before any write
  happens. The wire shapes are synthetic, derived from the reference
  implementation's handling of the plugin rather than a live capture
  (RedmineUP Agile is commercial) — see
  `crates/redmine-client/tests/fixtures/README.md`.
- `REDMINE_TAGS_ENABLED=true` makes `get_redmine_issue` report an issue's
  `tags` (AlphaNodes `additional_tags` plugin, each `{id, name}` with `id`
  frequently `null`) and lets `create_redmine_issue`/`update_redmine_issue`
  replace the full tag set via `tag_list` — no new tool, like Agile. A
  `tag_list` write always replaces the whole set (`[]` clears it); a tag name
  containing a comma is rejected rather than silently split, naming the
  array form. With the flag off, `tags` never appears and `tag_list` fails
  in-band with `MISCONFIGURED` before any write happens. Setting `tag_list`
  requires `create_issue_tags` or `edit_issue_tags` in `oauth` mode, on top
  of the tool's usual `add_issues`/`edit_issues` requirement. The wire shape
  is synthetic, derived from the reference implementation's handling of the
  plugin rather than a live capture — see
  `crates/redmine-client/tests/fixtures/README.md`.
- `manage_product`, registered only when `REDMINE_PRODUCTS_ENABLED=true`
  (RedmineUP Products plugin): `list`/`get`/`create`/`update` — the plugin
  exposes no delete endpoint. `list`/`get` work in read-only mode;
  `create`/`update` are blocked. Flat typed parameters replace the
  reference's untyped `fields` dict for `update`; an unknown parameter is
  rejected rather than silently dropped, and an added `offset` parameter
  reaches results past the reference's 100-item ceiling.
- `manage_contact`, registered only when `REDMINE_CRM_ENABLED=true`
  (RedmineUP CRM plugin): `list`/`get`/`create`/`update`/`delete`/
  `assign_to_project`/`remove_from_project`. `list`/`get` work in read-only
  mode; every other action is blocked. Same flat-typed-parameters and
  `offset` divergence as `manage_product`. Contact PII (`email`, `phone`,
  `address`, `birthday`, `website`) is returned to the caller unwrapped but
  never appears in a log line or an error message (errors reference
  `contact_id` only); display fields (name parts, `company`, `job_title`,
  `background`, `assigned_to`'s name) are boundary-wrapped.
  Both tools' wire shapes are synthetic, derived from the reference
  implementation's handling of the plugins rather than a live capture
  (both plugins are commercial) — see
  `crates/redmine-client/tests/fixtures/README.md`.
- `manage_document`, registered only when `REDMINE_DMSF_ENABLED=true` (the
  `redmine_dmsf` plugin — open source, GPL v2, but still needs a
  server-side install this project has not verified against): `list`/`get`/
  `create`/`update` — there is no `delete` action. `list`/`get` work in
  read-only mode; `create`/`update` are blocked. `create` accepts
  `content_base64` or `file_path` (reusing the existing upload-path
  resolver and size cap, not a second implementation) and validates an
  optional `version` string before ever uploading, so a malformed one sends
  zero requests; its response is deliberately sparse
  (`{document_id}` only), and the `note` field points at `action="get"` for
  full metadata. `update` always creates a new revision — it never replaces
  one — and always pre-fetches the document first so a missing `title`/
  `name` (which 500s the plugin's own endpoint) is never sent; a document
  with no revisions at all is `NOT_FOUND` with no write attempted. The
  update route is `/dmsf/files/{id}/revision/create.json` (a slash, not
  `dmsf_files/{id}`), and both write actions spell the stored filename
  `name` (not `filename`) and custom fields `custom_field_values` (not
  `custom_fields`), matching the plugin's own field names exactly. Same
  flat-typed-parameters divergence from the reference's untyped `fields`
  dict as `manage_product`/`manage_contact`. The wire shapes are synthetic,
  derived from the reference implementation's handling of the plugin rather
  than a live capture — see `crates/redmine-client/tests/fixtures/README.md`.
- `create_redmine_issue`/`update_redmine_issue` gain a `custom_fields`
  parameter — core Redmine, not plugin-gated, paying off a previously
  deferred decision (G8). Each entry gives exactly one of `id` (free) or
  `name` (matched case- and punctuation-insensitively via a project lookup,
  e.g. `"Story Points"` ≡ `"story_points"`; costs one extra request on
  create, two on update since that tool's parameters carry only
  `issue_id`), plus a `value` that is a string, an array of strings for a
  multi-value field, or `null` to clear it. An entry with neither/both of
  `id`/`name`, an unknown or ambiguous name, or a duplicate field id is
  rejected before any request reaches Redmine. Diverges from the
  reference's untyped `fields`/`extra_fields` dict (P4): an unresolvable key
  is a clear error, not silently dropped or passed through. A
  `custom_fields`-only update is accepted, not rejected as a no-op; an
  empty array is not.
- `REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS=true` recovers from a 422 naming a
  required issue custom field as blank or invalid: `create_redmine_issue`/
  `update_redmine_issue` retry the write **exactly once**, filled from the
  field's own `default_value` or from the new
  `REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS` map, and report what was filled
  as `autofilled_custom_fields`. A caller-supplied value Redmine rejected as
  empty or outside the field's allowed values is replaced the same way; a
  valid value is never touched. Never a guess from the field's other allowed
  values, never a second retry. With the flag off (the default, unchanged
  from G8's payoff above) or when nothing is fillable, the 422 still gains
  `missing_required_fields` and a hint. `REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS`
  fails the boot on invalid JSON, a non-object, or a non-string/array value;
  neither its keys nor its values ever reach a log line, `--print-config`, or
  `get_mcp_server_info` — only their count. Matching Redmine's validation
  message text is English-only.
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
  transport is now a startup error, matching `legacy-per-user`. See
  `docs/oauth-setup.md`.
- `oauth` mode now serves RFC 9728 protected-resource metadata at
  `/.well-known/oauth-protected-resource{mcp_path}` and RFC 8414
  authorization-server metadata (pointing at Redmine's real
  `/oauth/authorize`/`/oauth/token`/`/oauth/revoke`) at the `mcp_path`-suffixed
  well-known path by default, or at the root well-known path with
  `REDMINE_OAUTH_DISCOVERY_AS=self` (for clients that probe the canonical
  root location) — the two modes never both serve a document, so one always
  404s. Both documents' `scopes_supported` come from a new scope catalogue of
  Redmine Doorkeeper scope names, gated by `REDMINE_MCP_READ_ONLY` and the
  agile/tags plugin flags, and narrowable by the new `REDMINE_MCP_SCOPES`
  (an out-of-set entry refuses to boot, naming the accepted set). `admin` is
  never advertised. A new `POST /revoke` narrowly proxies RFC 7009 token
  revocation to Redmine, forwarding only `token`/`token_type_hint` plus the
  caller's own client authentication, and purges the revoked token from this
  server's introspection cache so it stops working on the very next request
  rather than after the cache TTL. `/readyz` in `oauth` mode now probes
  introspection with a synthetic token (bypassing the token cache) instead of
  reporting `not_probed`, gaining a `checks.introspection` field
  (`ok`/`misconfigured`/`unreachable`). All of the above are unauthenticated
  and unaffected by the bearer-auth middleware.
- `oauth` mode now enforces per-tool scopes: `tools/list` shows only the
  tools a bearer token's scopes permit (with `cache_scope: "private"` once
  filtering is active, since the list is per-token), and `tools/call` on a
  hidden tool is refused in-band with `INSUFFICIENT_SCOPE`, naming the
  missing scope(s), before any Redmine request is made. The map
  (`auth::scope::TOOL_SCOPES`) is verified against this server's own
  `redmine-client` call sites, not transcribed from the reference; a token
  carrying `admin` bypasses it entirely, and an unmapped tool is denied by
  default. `update_redmine_issue` carries a notes-only carve-out
  (`add_issue_notes` instead of `edit_issues` when only `notes`/
  `private_notes` change) and requires `manage_subtasks` in addition when
  reparenting. The new `REDMINE_OAUTH_SCOPE_ENFORCEMENT=off` restores the
  prior unfiltered behaviour for tokens minted before scopes existed,
  logging a startup `WARN`.
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
- A **payload-safety logging floor**: whatever `--log-level`/`RUST_LOG`
  requests is combined with a fixed cap on `rmcp`, `hyper`, `hyper_util`,
  `h2`, `reqwest`, `rustls`, and `wiremock` at `info` (and `tower_http::trace`
  at `debug`, metadata only) — the dependencies whose own `DEBUG`/`TRACE`
  output would otherwise include a tool call's arguments or a whole
  JSON-RPC envelope, nothing this server's own code can redact after the
  fact. This server's own code is never floored; naming a floored target
  explicitly (`RUST_LOG=trace,rmcp=trace`) still lifts it, logging a startup
  `WARN` naming the override.
- Rate limiting on the HTTP transport, on by default
  (`REDMINE_MCP_RATE_LIMIT_ENABLED`): a **standard** token-bucket class on
  `/mcp`/`/files/{uuid}` (`REDMINE_MCP_RATE_LIMIT_RPS`/`_BURST`, default 10
  rps / burst 40) and a stricter **strict** class on the `oauth-proxy` flow
  routes (`REDMINE_MCP_RATE_LIMIT_AUTH_RPS`/`_BURST`, default 1 rps / burst
  10, since those routes are attacker-attractive). Both key by peer IP —
  never `X-Forwarded-For`/`X-Real-IP`, which a client could set itself to
  bypass the limiter — except the standard class, which keys `/mcp` by a
  bearer token's digest instead of IP when one is present, so distinct
  users behind one NAT or proxy don't share a bucket.
  `REDMINE_MCP_RATE_LIMIT_MAX_KEYS` bounds each class's bucket map, evicting
  the least-recently-touched key at capacity. A rejected request gets `429
  {"error": "rate_limited"}` with `Retry-After` and `Cache-Control: no-store`;
  `/livez`, `/readyz`, and `/health` are never rate limited.
- One `tool_call` span per `tools/call`, on both transports, closed by a
  single event carrying the tool name, a process-local `request_id` (not a
  client-supplied or W3C trace id — nothing propagates it over the wire),
  `outcome` (`ok`/`error`/`denied`/`panic`) plus `code` when not `ok`, and
  `duration_ms` — never an argument value or key.
  `REDMINE_MCP_LOG_FORMAT` (`text`, default, or `json`, one object per line)
  changes only how a line is written, never what is in it.
- A distroless, non-root (`Dockerfile`), locally-built container image
  (`docker build --platform linux/arm64 …`; no registry push, no multi-arch
  matrix), plus a `docker-compose.yml` for a locked-down run with a
  read-only root filesystem and a named volume for the attachments
  directory. A new `--healthcheck` CLI flag GETs `/livez` and exits
  `0`/`1` (distroless has no shell or `curl` for a `HEALTHCHECK` to invoke
  another way); the image's own `HEALTHCHECK` runs it, deliberately never
  probing `/readyz`, so a Redmine outage cannot turn into a container
  restart loop.

### Fixed

- Tool schemas no longer advertise the non-standard `uint32`/`uint64`
  integer `format` values `schemars` emits for `u32`/`u64` fields, which made
  strict JSON Schema clients (e.g. opencode's Ajv-based validator) log an
  "unknown format" warning per field on every `tools/list`.
- A panicking tool handler used to hang the caller's request forever (the
  panic was caught by the tokio runtime, but nothing ever answered the
  client). `call_tool` now catches the panic and returns an in-band
  `{error, code: "INTERNAL", retryable: false, hint}` result instead; the
  session and every other in-flight request are unaffected.

### Changed

- The default HTTP bind is `127.0.0.1:8000`, not `0.0.0.0:8000`.
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
- The release build now sets `lto = "thin"`, `codegen-units = 1`, and
  `strip = "debuginfo"` explicitly, shrinking the binary by roughly a
  quarter; `panic = "unwind"` is pinned (it was already the default) since
  the panic containment above depends on it.

### Security

- Bumped `h2` to 0.4.16, fixing an advisory where empty `DATA` frames were
  accepted and queued without limit (unbounded memory, or a panic on length
  overflow) — pulled in transitively through `axum`, `reqwest`, `rmcp`, and
  `wiremock` alike.

# Troubleshooting

Operator-facing symptoms and their causes.

## `403` on `/mcp`

**Symptom:** every request to the streamable-HTTP endpoint gets a bare
`403`, before any MCP handshake happens.

**Cause:** the `Host` (and, for browser-originated requests, `Origin`)
allowlist rejected the request. `REDMINE_MCP_ALLOWED_HOSTS` is derived
automatically from `SERVER_HOST`: a loopback bind (the default) allows
`localhost`/`127.0.0.1`/`::1`; a non-loopback bind (`0.0.0.0`, a real
interface address) requires `PUBLIC_HOST` so the server knows the hostname
clients actually use — startup refuses to guess. `REDMINE_MCP_ALLOWED_ORIGINS`
is a separate, explicit list for browser clients; `"*"` is rejected there
outright, since it would let any website drive the server through a user's
browser.

**Fix:** set `PUBLIC_HOST` (and `PUBLIC_PORT`/`PUBLIC_SCHEME` if a proxy
changes them) to the hostname clients connect through, or list the exact
values in `REDMINE_MCP_ALLOWED_HOSTS`/`REDMINE_MCP_ALLOWED_ORIGINS`.
`REDMINE_MCP_ALLOWED_HOSTS=*` disables Host validation entirely and is a
last resort — it removes the only signal that distinguishes a legitimate
request from DNS rebinding, and is only safe when a reverse proxy in front
of this server already validates `Host` itself.

## `413`

**Symptom:** a request with a large body (typically `upload_file` sending
`content_base64`) is rejected before reaching the tool.

**Cause:** `REDMINE_MCP_MAX_REQUEST_BODY_BYTES` (default 4 MiB, range 1 KiB
to 64 MiB), enforced mid-stream inside rmcp's own HTTP transport, not by a
separate body-limit middleware.

**Fix:** raise the limit if the deployment genuinely needs larger uploads, or
have the client send `file_path`/a smaller file instead of inlining base64.

## `401` with a `WWW-Authenticate` header

**Symptom:** a bearer-authenticated call is rejected with `401`, and the
response carries a `WWW-Authenticate: Bearer resource_metadata="..."` header
pointing at a discovery document.

**Cause:** this is the intended discovery signal, not a misconfiguration —
the client is expected to fetch that
`/.well-known/oauth-protected-resource` URL, follow it to the authorization
server's own `/.well-known/oauth-authorization-server` metadata, and
(re-)authenticate. In `oauth` mode that document names Redmine's own
`/oauth/authorize`/`/oauth/token`/`/oauth/revoke` endpoints; in
`oauth-proxy` mode it names this server's own `/authorize`/`/token`/
`/register`/`/revoke` endpoints instead (with
`token_endpoint_auth_methods_supported: ["none"]`, since every
dynamically-registered client is public).

**Fix:** if the client is not following discovery at all, check its MCP
client library supports RFC 9728 discovery; if it is authenticating against
the wrong endpoint set, confirm `REDMINE_AUTH_MODE` (`oauth` vs
`oauth-proxy`) matches what the client was configured to expect.

## `INSUFFICIENT_SCOPE`, or a tool is missing from `tools/list`

**Symptom:** a tool call returns an in-band `{code: "INSUFFICIENT_SCOPE"}`
error, or a tool the caller expects simply does not appear in `tools/list`.

**Cause:** both are the same per-tool scope table (`TOOL_SCOPES`) applied
twice — once to filter `tools/list` down to what the held token could
successfully call, and once to actually enforce a call. A token missing a
required scope never sees the tool listed, and a call to it (by name,
bypassing the list) gets the in-band error instead of a silent no-op.

**Fix:** re-authorize with the missing scope(s), or use a different tool.
`get_mcp_server_info`'s `oauth_scope_enforcement` field confirms whether
enforcement is on at all.

## `MISCONFIGURED` on an agile or tag parameter

**Symptom:** setting `story_points`/`agile_sprint_id`/`agile_position` on
`update_redmine_issue`, or `tag_list` on `create_redmine_issue`/
`update_redmine_issue`, returns `{code: "MISCONFIGURED"}` instead of
reaching Redmine.

**Cause:** the corresponding plugin flag (`REDMINE_AGILE_ENABLED` for the
agile fields, `REDMINE_TAGS_ENABLED` for `tag_list`) is off. This is a
deliberate in-band error rather than a hidden parameter or a de-registered
tool: `update_redmine_issue`/`create_redmine_issue` themselves are always
registered, and only these specific parameters are gated, so hiding the
whole tool would be the wrong granularity.

**Fix:** set the flag if the plugin is actually installed on the target
Redmine, or omit the parameter.

## `READ_ONLY`

**Symptom:** a write call returns `{code: "READ_ONLY"}`, or a write-only
tool has vanished from `tools/list` entirely.

**Cause:** `REDMINE_MCP_READ_ONLY=true`. Tools that are *always* a write
(`create_redmine_issue`, `upload_file`, `manage_time_entry`, …) are removed
from the router outright, so calling one by name fails as "tool not found".
Tools with a mix of read and write actions (`manage_issue_relation`,
`manage_product`, `manage_contact`, `manage_document`, …) stay registered —
their read actions (`list`/`get`) keep working, and only the write actions
refuse with `READ_ONLY`.

**Fix:** this is working as configured; disable read-only mode if the
deployment is meant to allow writes.

## `429`

**Symptom:** a call to `/mcp` or to an oauth-proxy flow route (`/register`,
`/authorize`, `/auth/callback`, `/token`, `/revoke`) gets `429
{"error": "rate_limited"}` with a `Retry-After` header.

**Cause:** `REDMINE_MCP_RATE_LIMIT_ENABLED` (on by default) applies two
independent limits: a standard class on `/mcp`/`/files/{uuid}`
(`REDMINE_MCP_RATE_LIMIT_RPS`/`_BURST`, default 10 rps / burst 40) and a
much stricter class on the oauth-proxy flow routes
(`REDMINE_MCP_RATE_LIMIT_AUTH_RPS`/`_BURST`, default 1 rps / burst 10),
since those routes are attacker-attractive (registration, token exchange).
A single legitimate client can hit either ceiling during a burst of calls or
a multi-step DCR/authorization flow, with no proxy involved at all.

**Fix:** raise the relevant `REDMINE_MCP_RATE_LIMIT_*`/`_AUTH_*` variable if
the deployment's legitimate traffic needs a higher ceiling.

### Everything gets `429` behind my reverse proxy

**Symptom:** every client behind a load balancer, NAT gateway, or
TLS-terminating reverse proxy starts getting `429` from `/mcp`, even though
no single client is sending anywhere near the configured rate.

**Cause:** the rate limiter keys by the request's peer IP address only — it
deliberately never reads `X-Forwarded-For` or `X-Real-IP`, since either
header is attacker-controllable input a client could set itself to bypass
the limiter entirely. If every client's connection to this server arrives
from one address (a proxy's own egress IP), they all share one bucket.

**Fix:** rate-limit at the proxy, which can see real client addresses,
rather than raising this server's limits to compensate — a higher limit
here would also raise the ceiling for a single misbehaving client sharing
that address. If the deployment authenticates with `oauth`/`oauth-proxy`
bearer tokens, `/mcp` requests already key by token digest instead of IP
when a token is present, which sidesteps this for that route; the strict
class (`/register` &c.) and any legacy-mode request with no bearer token
still key by IP only.

## `INTERNAL`

**Symptom:** a tool call returns `{code: "INTERNAL", retryable: false}`
with a generic "the server encountered an internal error" message.

**Cause:** the tool's handler panicked. The panic is caught before it can
take down the request (and is logged server-side via `tracing::error!` with
the tool name and panic message — never returned to the caller), but the
underlying bug is still there.

**Fix:** this is a bug report, not a retry — the same call will panic
again. File it with the tool name, arguments (redacted of secrets), and the
server-side log line.

## A stdio client sees corrupt or unparseable JSON-RPC output

**Symptom:** an stdio-transport client fails to parse a line from the
server's stdout, or the connection desyncs entirely.

**Cause:** on stdio, stdout is the JSON-RPC wire — any stray `println!` or
library that writes to stdout corrupts the stream for every subsequent
message. The project guards against this with a deny-level clippy lint on
`print!`/`println!` and an end-to-end test that spawns the real binary and
asserts every stdout line is valid JSON, so a regression here should not
reach a release. If it does anyway, it points at a new dependency or code
path that writes to stdout directly.

**Fix:** run with `--transport http` as a workaround while filing the issue;
the HTTP transport has no such constraint since stdout isn't the wire.

## `oauth-proxy` clients must re-register after a restart

**Symptom:** every dynamically-registered OAuth client (and every active
authorization session) stops working immediately after the server process
restarts, and clients have to go through Dynamic Client Registration again.

**Cause:** by design (see the ADR for `oauth-proxy`'s authorization-server
role), all proxy state — registered clients, authorization codes, refresh
tokens — is in-memory only, with no persistence and no shared state across
replicas. A restart drops all of it.

**Fix:** none needed — this is the same `401` challenge-and-reauthorize flow
a client already implements for ordinary token expiry, so a well-behaved
MCP client recovers on its own. Avoid restarting the process during a burst
of user-facing authorization flows if that's disruptive; there is no way to
preserve state across a restart in this mode today.

## A refresh token "stopped working"

**Symptom:** a client that stored an old refresh token gets `invalid_grant`
on `/token`, and its whole session (not just that refresh) is now dead.

**Cause:** `oauth-proxy` refresh tokens rotate on every use, and reusing an
already-rotated (retired) token is treated as evidence of token theft or a
client bug that duplicated a request — the server responds by revoking the
entire session upstream, not just rejecting the stale token. The same
containment applies to a refresh token that is *still* current: redemption
is single-use and atomic, so two requests presenting the same refresh token
at the same time are indistinguishable from an attacker racing the
legitimate client — both fail and the session dies.

**Fix:** the client must always use the *most recent* refresh token
returned, never a cached older one, and must serialize its refreshes —
never fire a speculative parallel refresh alongside one already in flight;
if this happens after a genuine crash mid-refresh (the client can't tell if
its own request succeeded), the fix is to re-authorize from scratch, not to
retry the old refresh token.

## `UNEXPECTED_RESPONSE` on `get_redmine_attachment`

**Symptom:** `get_redmine_attachment` returns `{code: "UNEXPECTED_RESPONSE",
retryable: false}` instead of downloading the file, even though the
attachment's metadata looked fine.

**Cause:** the attachment's `content_url` (or a redirect Redmine sent while
fetching it) points at a different origin than `REDMINE_URL` — different
scheme, host, or port. This client only ever sends its credential to the
configured Redmine origin, so a Redmine that serves attachments from a CDN
or that redirects `http` to `https` is refused rather than leaking the
credential to that other origin.

**Fix:** set `REDMINE_URL` to the origin attachments are actually served
from (e.g. the `https` form if Redmine redirects to it).

## Attachment `404`

**Symptom:** `GET /files/{uuid}` (or `get_redmine_attachment` referencing a
previously uploaded id) returns `404 not found` for an id that worked
earlier.

**Cause:** attachments expire after `ATTACHMENT_EXPIRES_MINUTES` (default
60 minutes) and are swept from disk by a periodic cleanup task
(`CLEANUP_INTERVAL_MINUTES`, default every 15 minutes, disabled entirely by
`AUTO_CLEANUP_ENABLED=false`). This store is meant to bridge an upload to a
Redmine attach call, not to serve as long-term file storage.

**Fix:** re-upload if the file is still needed; if attachments need a
longer lifetime for a specific workflow, raise
`ATTACHMENT_EXPIRES_MINUTES`.

## `PUBLIC_HOST` missing at startup in a container

**Symptom:** the container exits immediately on start with a `PUBLIC_HOST`
config error, even though the image's default `SERVER_HOST=0.0.0.0` looks
correct for a container.

**Cause:** a non-loopback bind (which `0.0.0.0` is) means the server cannot
derive a safe `Host` allowlist by itself — it needs to know the hostname
clients will actually use to reach it, which only the operator knows for a
given deployment.

**Fix:** set `PUBLIC_HOST` (the `docker-compose.yml` example already
requires it, defaulting to `localhost` for local use). Set it to the real
hostname or IP the container is reachable at in production.

## `REDMINE_MCP_ALLOW_UNAUTHENTICATED_NETWORK` missing at startup

**Symptom:** the process exits immediately with a
`REDMINE_MCP_ALLOW_UNAUTHENTICATED_NETWORK` config error, on a `legacy` auth
mode deployment that previously started fine on a non-loopback `SERVER_HOST`.

**Cause:** a shared `REDMINE_API_KEY` authenticates this server to Redmine,
not the caller to this server — anyone who can reach a non-loopback bind acts
as that Redmine account. This used to only log a `WARN`; it is now a startup
error so the risk cannot go unnoticed. This is a breaking change on upgrade
for any existing `legacy` + non-loopback HTTP deployment.

**Fix:** pick one — bind loopback (`SERVER_HOST=127.0.0.1`, the default), put
an authenticating proxy in front, switch to
`REDMINE_AUTH_MODE=legacy-per-user`/`oauth`/`oauth-proxy`, or set
`REDMINE_MCP_ALLOW_UNAUTHENTICATED_NETWORK=true` to accept the risk. See
docs/configuration.md#exposing-the-server-on-a-network.

## Redmine unreachable

**Symptom:** `/readyz` returns `503`, but the process is otherwise running
and `/livez` still returns `200`.

**Cause:** this is the intended split — `/livez` never checks Redmine (a
dependency blip must not be able to trigger a container restart storm),
while `/readyz` runs a TTL-cached probe (`HEALTH_INTROSPECTION_TTL_SECONDS`,
default 30s) against Redmine (or, in `oauth` mode, the introspection
endpoint) and reports `503` while it stays unreachable.

**Fix:** point an orchestrator's *restart* policy at `/livez` and its
*traffic-routing/readiness* policy at `/readyz` separately, exactly as
distinguished by their names; a `503` on `/readyz` alone should not restart
the container, only pull it out of rotation until Redmine recovers.

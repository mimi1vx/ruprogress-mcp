# OAuth (bearer-token) setup

`REDMINE_AUTH_MODE=oauth` — each MCP client presents its own Redmine
Doorkeeper access token as `Authorization: Bearer`, and this server validates
it by RFC 7662 introspection before forwarding it upstream verbatim. Every
user acts as themselves, with their own token's permissions.

## Status

**Live now:** bearer extraction, RFC 7662 introspection with
a digest-keyed cache, the `401 WWW-Authenticate: Bearer` / `503 Retry-After`
challenge, forwarding the validated token to Redmine, RFC 9728/RFC 8414
discovery documents, `POST /revoke`, an introspection-backed `/readyz` probe,
and per-tool scope enforcement: `tools/list` shows only the tools a token's
scopes permit, and `tools/call` on a hidden tool is refused in-band with
`INSUFFICIENT_SCOPE`.

`REDMINE_AUTH_MODE=oauth-proxy` is the other bearer-token mode: this server
*is* the authorization server MCP clients talk to, registering themselves
with RFC 7591 Dynamic Client Registration rather than being hand-registered
in Redmine's admin panel. See [oauth-proxy setup](#oauth-proxy-this-server-as-the-authorization-server)
below.

**Not live yet (`oauth-proxy`):** `refresh_token` handling and proxy-mode
`/revoke` (follow-up work) — until then, an expired proxy access token
means re-authorizing from `/authorize` again, and there is no way to
revoke one early.

## Scope enforcement

Every tool call is checked against a hand-maintained map from tool name to
the Redmine permission(s) it needs — `crates/ruprogress-mcp/src/auth/scope.rs`
(`TOOL_SCOPES`), the source of truth for both `tools/list` filtering and
`tools/call` denial. Adding a new tool means adding its row to that map in
the same change: an unmapped tool is denied by default and logged at
`ERROR`, and a CI anti-drift test
(`crates/ruprogress-mcp/tests/oauth_scopes.rs`) fails the build if a
registered tool has no entry.

Semantics:

- A tool's requirement is either fixed, or depends on its `action` argument
  (mirroring `manage_issue_relation`'s/`manage_redmine_wiki_page`'s existing
  per-action shape). The token must hold **every** scope in the matching
  requirement; an empty requirement means any authenticated token may call
  the tool.
- A token carrying the `admin` scope bypasses the map entirely, for both
  visibility and calls — matching Redmine's own admin semantics.
- `update_redmine_issue` has a notes-only carve-out: a call whose changed
  fields are only `notes`/`private_notes`, with no `uploads`, needs
  `add_issue_notes` instead of `edit_issues`; reparenting (`parent_issue_id`)
  additionally needs `manage_subtasks`. A token holding either `edit_issues`
  or `add_issue_notes` sees the tool in `tools/list`.
- A `tools/call` denial returns the in-band envelope
  `{error, code: "INSUFFICIENT_SCOPE", retryable: false, hint}`, naming the
  missing scope(s) — never a protocol-level error, and never "tool not
  found" even though the tool is hidden from `tools/list`.
- Argument-conditional Redmine permissions that do not map to a distinct MCP
  parameter shape (e.g. `tag_list` writes, agile-board fetches) are left to
  Redmine's own enforcement: the call reaches Redmine and comes back as a
  normal in-band `403`, exactly as it would through Redmine's own UI.

`REDMINE_OAUTH_SCOPE_ENFORCEMENT=off` (default `on`) disables both
`tools/list` filtering and `tools/call` denial, restoring unfiltered
behaviour, and logs a startup `WARN`. This exists only for the documented
migration case: tokens minted before the OAuth application advertised
scopes introspect with an empty scope set and would otherwise be denied
everything.

```bash
REDMINE_OAUTH_SCOPE_ENFORCEMENT=off
```

A client that caches `tools/list` across users despite the `cache_scope:
"private"` hint this server sends once filtering is active will show the
wrong list to the wrong user — this server cannot fix a client that ignores
that hint.

## Step 1: Register an OAuth app for your users

1. Log in to Redmine as admin → **Administration → Applications** → **New
   Application**.
2. Fill in:
   - **Name:** anything recognisable, e.g. `MCP Server`.
   - **Redirect URI:** whatever your MCP client's own OAuth flow uses (this
     server does not participate in that flow — it only validates the
     resulting token).
   - **Confidential:** Yes.
3. Save and note the **Client ID** and **Client Secret** — your MCP client
   needs these to obtain tokens; this server never sees them.

## Step 2: Register a Doorkeeper introspection client

This server validates incoming bearer tokens by calling Doorkeeper's RFC 7662
introspection endpoint (`POST /oauth/introspect`), authenticating itself with
its own confidential OAuth application. This can be the same application as
Step 1, or a separate one for independent credential rotation.

### 2a. Register the application

1. **Administration → Applications → New Application.**
2. Fill in:
   - **Name:** `Redmine MCP Server (introspection)`.
   - **Redirect URI:** `urn:ietf:wg:oauth:2.0:oob` (unused, but required by
     the form — this client never performs an authorization-code flow).
   - **Confidential:** Yes.
   - **Scopes:** leave empty.
3. Save and note the **Client ID** and **Client Secret**.

> If **Administration → Applications** 403s: enable **Administration →
> Settings → API → "Enable REST web service"** first (or, from a Rails
> console, `Setting.rest_api_enabled = "1"`).

### 2b. Enable cross-app token introspection in Doorkeeper

Redmine ships with `allow_token_introspection false` hard-coded, so even an
authenticated introspection client cannot introspect a token issued to a
*different* OAuth app — exactly the case here, since the introspection client
(Step 2a) must introspect tokens issued to the user-flow app (Step 1).

**Edit Redmine's own initializer in place, on the Redmine server:**
`config/initializers/30-redmine.rb`. Find:

```ruby
    allow_token_introspection false
```

Replace it with:

```ruby
    allow_token_introspection do |_token, authorized_client, _resource_owner|
      !authorized_client.nil? && authorized_client.confidential?
    end
```

This grants introspection rights to any confidential OAuth client. Restart
Redmine after the change.

> **Why edit `30-redmine.rb` directly instead of a new initializer?** Redmine
> wraps its Doorkeeper configuration in a `Rails.application.config.to_prepare
> do ... end` block, and `Doorkeeper.configure do ... end` **rebuilds the
> entire config from scratch** on every call rather than merging. A second
> `Doorkeeper.configure` block in your own initializer would silently wipe
> Redmine's `admin_authenticator`, `resource_owner_authenticator`,
> `grant_flows`, and everything else it sets — the visible symptom is
> **Administration → Applications** 403ing with a log line about
> `admin_authenticator being unconfigured`. Track this edit as a deployment
> patch (e.g. a Dockerfile `RUN sed -i ...` step) so it survives Redmine
> upgrades.

### 2c. Verify

```bash
curl -X POST "$REDMINE_URL/oauth/introspect" \
  -u "$REDMINE_INTROSPECT_CLIENT_ID:$REDMINE_INTROSPECT_CLIENT_SECRET" \
  -d "token=any-test-token&token_type_hint=access_token"
```

Expect `200 {"active":false}` — the `false` is correct (the token is made
up); what matters is the `200`.

| Response | Meaning |
|---|---|
| `404 Page not found` | The route isn't mounted. Confirm step 2b was applied and Redmine was restarted. |
| `401 invalid_client` | The introspection client's id/secret are wrong, or it isn't confidential. |
| `200 {"active": false}` for a *known-valid* token | `allow_token_introspection` is returning falsy for this client — recheck 2b. |

## Step 3: Configure ruprogress-mcp

```bash
REDMINE_AUTH_MODE=oauth
REDMINE_URL=https://redmine.example.com
REDMINE_MCP_BASE_URL=https://mcp.example.com   # this server's own public URL

# Introspection client (Step 2)
REDMINE_INTROSPECT_CLIENT_ID=<Client ID from Redmine>
REDMINE_INTROSPECT_CLIENT_SECRET=<Client Secret from Redmine>
# Or: REDMINE_INTROSPECT_CLIENT_SECRET_FILE=/run/secrets/redmine_introspect_client_secret

# Optional: positive-introspection cache TTL, 0..3600 seconds (default 60).
# 0 disables caching. No upstream counterpart — a ruprogress-mcp addition.
# REDMINE_OAUTH_TOKEN_CACHE_TTL_SECONDS=60

# Optional: /readyz's introspection-probe cache TTL in seconds (default 30).
# HEALTH_INTROSPECTION_TTL_SECONDS=30
```

`oauth` mode requires `--transport http` (or `FASTMCP_TRANSPORT=http`): there
is no per-request header, and nothing to challenge with, on stdio. Missing
`REDMINE_INTROSPECT_CLIENT_ID`/`REDMINE_INTROSPECT_CLIENT_SECRET`, or `oauth`
on stdio, both refuse to start with a message naming the variable.

## Step 4: Start and verify

```bash
cargo run -- --transport http
```

```bash
# No token: 401 with a WWW-Authenticate challenge naming the
# resource-metadata document below.
curl -i http://127.0.0.1:8000/mcp

# A real token, obtained however your MCP client obtains one:
curl -i http://127.0.0.1:8000/mcp \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}'
```

Verify discovery and readiness, both unauthenticated:

```bash
# RFC 9728 protected-resource metadata
curl http://127.0.0.1:8000/.well-known/oauth-protected-resource/mcp

# RFC 8414 authorization-server metadata (suffixed path; see the
# "Cursor and self-AS discovery" section below for the root-path variant)
curl http://127.0.0.1:8000/.well-known/oauth-authorization-server/mcp

# /readyz probes Doorkeeper introspection in oauth mode
curl http://127.0.0.1:8000/readyz
# {"status":"ready","redmine":"ok","checks":{"introspection":"ok"},"checked_at":"..."}
```

If `/readyz` reports `"status":"not_ready"` with `"introspection":"misconfigured"`,
the introspection client is misconfigured — recheck Step 2 (confidential,
`allow_token_introspection` applied). `"unreachable"` means the endpoint could
not be reached at all (network, `REDMINE_URL`, or Redmine itself down).

### Endpoints exposed in `oauth` mode

| Endpoint | Standard | Purpose |
|---|---|---|
| `GET /.well-known/oauth-protected-resource{mcp_path}` | RFC 9728 §3.1 | Tells clients where to find the authorization server |
| `GET /.well-known/oauth-authorization-server{mcp_path}` (or the root path — see below) | RFC 8414 | Advertises Redmine's Doorkeeper endpoints, scoped to this MCP resource |
| `POST /revoke` | RFC 7009 | Revokes an OAuth2 token (proxies to Redmine's `/oauth/revoke`, and purges the token from this server's introspection cache) |

Redmine ships the [Doorkeeper](https://github.com/doorkeeper-gem/doorkeeper)
gem for OAuth2 but does not serve the RFC 8414 discovery document itself; this
server serves path-scoped metadata on Redmine's behalf, pointing
`authorization_endpoint`/`token_endpoint`/`revocation_endpoint` at Redmine's
real `/oauth/authorize`, `/oauth/token`, and `/oauth/revoke`. `POST /revoke`
is separate from `revocation_endpoint`: it is a narrow, field-allowlisting
proxy mounted on this server itself, for clients that expect a revocation
endpoint at the resource server rather than the authorization server. It
accepts only `application/x-www-form-urlencoded` bodies up to 8&nbsp;KiB, and
forwards only `token`/`token_type_hint` plus the caller's own client
authentication (an `Authorization: Basic` header, or `client_id`/
`client_secret` form fields) — never this server's own introspection
credential.

### Cursor and self-AS discovery

Some MCP clients (for example Cursor) discover the authorization server by
probing its canonical RFC 8414 well-known location, `/.well-known/oauth-authorization-server`
(no suffix). Because the default `redmine` discovery mode names Redmine as the
authorization server and serves the document at the `/mcp`-suffixed path only,
those clients fail discovery.

Set `REDMINE_OAUTH_DISCOVERY_AS=self` to have this server advertise itself as
the authorization server instead: the RFC 8414 document moves to the root
well-known path with `issuer = REDMINE_MCP_BASE_URL`, and the suffixed path
404s. `authorization_endpoint`/`token_endpoint`/`revocation_endpoint` still
point directly at Redmine either way — this server issues no tokens itself.
This mode is opt-in; the default `redmine` mode is unchanged.

```bash
REDMINE_OAUTH_DISCOVERY_AS=self
```

If your Redmine OAuth Application (Step 1) enables only a subset of
permissions, set `REDMINE_MCP_SCOPES` to that subset (space-separated) so the
advertised `scopes_supported` matches what the Application can grant and
consent does not fail with `invalid_scope`:

```bash
REDMINE_MCP_SCOPES="view_project view_issues add_issues edit_issues"
```

`REDMINE_MCP_SCOPES` is a subset of the scopes this server already advertises
(the Redmine permissions its tools actually use), not a mirror of the
Application's full permission list; an out-of-set entry refuses to boot with
the full accepted set listed in the error. `admin` is never advertised —
tokens with admin scope bypass Redmine's own per-permission checks, so
advertising it by default would make every consent screen request full
administrative access.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Every request is `401` with no `Authorization` header sent | Client isn't attaching a bearer token | Confirm the client actually obtained one from Redmine and is sending `Authorization: Bearer <token>` |
| `401` with `error="invalid_token"` | Token is inactive, revoked, or expired per introspection | Re-authorize; test directly with the Step 2c `curl` command against the real token |
| Every request is `503` | Introspection endpoint unreachable, misconfigured, or unmounted | Re-run Step 2c's verification `curl`; check `REDMINE_URL` and the introspection credentials |
| `/readyz` reports `"introspection":"misconfigured"` | Introspection client rejected by Doorkeeper | Recheck Step 2: the client must be confidential and `allow_token_introspection` (2b) applied |
| `/readyz` reports `"introspection":"unreachable"` | Introspection endpoint unreachable | Check `REDMINE_URL` and network connectivity from this server to Redmine |
| Discovery endpoints `404` | Not in `oauth` mode, wrong path, or the wrong discovery mode's canonical path | Ensure `REDMINE_AUTH_MODE=oauth`; the suffixed path 404s in `self` discovery mode and vice versa |
| Server refuses to start naming `REDMINE_INTROSPECT_CLIENT_ID`/`_SECRET` | `oauth` mode requires them | Register the introspection client per Step 2 |
| Server refuses to start naming `REDMINE_MCP_SCOPES` | An entry is not in the current mode's advertised set | Use one of the scopes the error message lists as accepted |
| Server refuses to start with a `Conflict` naming the transport | `oauth` mode was requested on `stdio` | Use `--transport http` |
| Token works directly against Redmine but not through this server | Wrong `REDMINE_URL`, or a proxy stripping the `Authorization` header | In Docker, use the internal hostname (e.g. `http://redmine:3000`); check for a proxy that drops `Authorization` |

## oauth-proxy: this server as the authorization server

Plain `oauth` mode requires every MCP client to be hand-registered in
Redmine's admin panel with its exact redirect URI — fine for a known client
list, awkward for a client that expects to discover and register itself.
`REDMINE_AUTH_MODE=oauth-proxy` closes that gap: MCP clients discover *this*
server as their authorization server, register via RFC 7591 Dynamic Client
Registration, and run authorization-code + PKCE against this server's own
`/authorize` and `/token`, which in turn drive a second, independent
authorization-code + PKCE flow against Redmine using one
operator-registered upstream OAuth application. Redmine's own authorize
page remains the consent screen — this server renders no HTML and sets no
cookies in any mode.

The token an MCP client ends up holding is an opaque `rup_at_`-prefixed
handle minted by this server, never the upstream Redmine token: presenting
the upstream token directly to `/mcp` in this mode is always `401`. A proxy
token resolves to the upstream token on every request and is verified via
the same RFC 7662 introspection `oauth` mode uses, so scope enforcement and
`INSUFFICIENT_SCOPE` denial behave identically once a client holds one.

### Configure

```bash
REDMINE_AUTH_MODE=oauth-proxy
REDMINE_URL=https://redmine.example.com
REDMINE_MCP_BASE_URL=https://mcp.example.com   # this server's own public URL

# Introspection client — same requirements as oauth mode (see Steps 1-2
# above): a confidential Redmine OAuth application with
# allow_token_introspection enabled.
REDMINE_INTROSPECT_CLIENT_ID=<Client ID from Redmine>
REDMINE_INTROSPECT_CLIENT_SECRET=<Client Secret from Redmine>

# Optional: a dedicated upstream OAuth application for the authorization-code
# flow this server will run against Redmine on a client's behalf. Defaults to
# the introspection client above when both are left unset; setting one
# without the other is a startup Conflict. The application needs the
# authorization-code grant and ${REDMINE_MCP_BASE_URL}/auth/callback
# registered as its redirect URI, plus use_refresh_token if you want refresh
# tokens to work (refresh support in this server itself is follow-up work).
# REDMINE_OAUTH_CLIENT_ID=<upstream Client ID>
# REDMINE_OAUTH_CLIENT_SECRET=<upstream Client Secret>

# Optional: which redirect URIs a DCR client may register (comma/whitespace
# separated scheme://host[:port]/path* patterns, or the literal "*" for no
# restriction beyond http(s)-only). Default: loopback only
# (http://localhost:*, http://127.0.0.1:*) — the right choice for CLI/desktop
# MCP clients and the only setting that needs no further thought.
# REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS=https://your-hosted-client.example.com/callback
```

`REDMINE_OAUTH_DISCOVERY_AS` does not apply here: this server always
advertises itself as the authorization server in `oauth-proxy` mode, at the
root well-known path. Explicitly setting it to `redmine` is a startup
`Conflict`. `REDMINE_MCP_JWT_SIGNING_KEY`/`_FILE`, if set, is accepted and
ignored with a startup `WARN` — proxy tokens here are opaque server-side
handles, never signed JWTs, so there is nothing for a signing key to do.

`oauth-proxy`, like `oauth`, requires `--transport http`.

### Register a client and verify discovery

```bash
cargo run -- --transport http
```

```bash
# RFC 8414 authorization-server metadata, at the root path (not the
# /mcp-suffixed one — that 404s in this mode).
curl http://127.0.0.1:8000/.well-known/oauth-authorization-server
# {"issuer":"...","authorization_endpoint":"...../authorize",
#  "token_endpoint":"...../token","registration_endpoint":"...../register",
#  "token_endpoint_auth_methods_supported":["none"], ...}

# RFC 7591 Dynamic Client Registration — no credential needed.
curl -X POST http://127.0.0.1:8000/register \
  -H 'content-type: application/json' \
  -d '{"redirect_uris": ["http://localhost:4000/callback"]}'
# {"client_id":"...","token_endpoint_auth_method":"none",
#  "redirect_uris":["http://localhost:4000/callback"], ...}
# Note: no client_secret is ever issued — every client registered here is
# public (PKCE plus the redirect allowlist is the real control).
```

A redirect URI outside the allowlist is rejected with
`{"error": "invalid_redirect_uri"}`; malformed or unsupported client
metadata (e.g. `"token_endpoint_auth_method": "client_secret_post"`, which
this server never accepts) is `{"error": "invalid_client_metadata"}`.

### Endpoints exposed in `oauth-proxy` mode

| Endpoint | Standard | Purpose |
|---|---|---|
| `GET /.well-known/oauth-protected-resource{mcp_path}` | RFC 9728 §3.1 | Names this server as the resource **and** points at itself as the authorization server |
| `GET /.well-known/oauth-authorization-server` (root only) | RFC 8414 | Advertises this server's own `/authorize`, `/token`, `/register`, `/revoke` |
| `POST /register` | RFC 7591 | Dynamic Client Registration — open, no initial access token, every client public |
| `GET /authorize` | RFC 6749 §3.1 | Validates the client/redirect URI/PKCE, then redirects to Redmine's own `/oauth/authorize` behind a second, independently generated upstream PKCE pair |
| `GET /auth/callback` | — (this server's own) | Where Redmine redirects back to; exchanges Redmine's code for an upstream token and redirects to the client with this server's own code |
| `POST /token` | RFC 6749 §4.1.3 | Redeems that code (mandatory S256 PKCE) for a `rup_at_` proxy access token — never the upstream Redmine token |

## Known limitation: in-memory, single-replica state

Every DCR registration, in-flight authorization transaction, authorization
code, and issued proxy token lives in this process's memory only — nothing
is persisted, and nothing is shared between replicas. A restart drops
everything: clients re-register and re-authorize, driven by the same `401`
challenge they already handle. Behind a load balancer with more than one
replica, a flow that starts on one instance and continues on another (e.g.
`/authorize` on instance A, `/auth/callback` on instance B) fails. Bind
`oauth-proxy` to a single instance, or route by a sticky session, until a
durable backend exists.

## Known limitation: no audience binding

Doorkeeper introspection returns no `aud` claim, so this server cannot verify
that a presented token was issued *for it* specifically rather than for
another OAuth client of the same Redmine instance. Any holder of any valid
Redmine access token can drive this server as that user. This is not fixable
without changes upstream in
Redmine/Doorkeeper; it is bounded by the fact that the token grants no *more*
against this server than it already grants directly against Redmine's own
REST API. See `docs/adr/0008-oauth-resource-server.md`.

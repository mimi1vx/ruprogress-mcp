# OAuth (bearer-token) setup

`REDMINE_AUTH_MODE=oauth` — each MCP client presents its own Redmine
Doorkeeper access token as `Authorization: Bearer`, and this server validates
it by RFC 7662 introspection before forwarding it upstream verbatim. Every
user acts as themselves, with their own token's permissions.

Ported from the upstream reference server's `docs/oauth-setup.md`
(`jztan/redmine-mcp-server`, branch `develop`, captured 2026-08-09), trimmed
to what `ruprogress-mcp` actually implements today.

## Status

**Live now (phases 6b1–6b2):** bearer extraction, RFC 7662 introspection with
a digest-keyed cache, the `401 WWW-Authenticate: Bearer` / `503 Retry-After`
challenge, forwarding the validated token to Redmine, RFC 9728/RFC 8414
discovery documents, `POST /revoke`, and an introspection-backed `/readyz`
probe.

**Not live yet:**

- Per-tool scope enforcement (`REDMINE_OAUTH_SCOPE_ENFORCEMENT`) and
  `tools/list` filtering — phase 6b3. Every token that introspects as active
  and unexpired can call every tool today, regardless of its `scope`.
  `REDMINE_MCP_SCOPES` (below) narrows what is *advertised*, not what is
  *enforced*, until then.
- `oauth-proxy` mode (this server acting as an authorization server with
  Dynamic Client Registration) — out of scope for v1.0 entirely.

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

## Known limitation: no audience binding

Doorkeeper introspection returns no `aud` claim, so this server cannot verify
that a presented token was issued *for it* specifically rather than for
another OAuth client of the same Redmine instance. Any holder of any valid
Redmine access token can drive this server as that user. This is also the
reference server's behaviour and is not fixable without changes upstream in
Redmine/Doorkeeper; it is bounded by the fact that the token grants no *more*
against this server than it already grants directly against Redmine's own
REST API. See `docs/adr/0008-oauth-resource-server.md`.

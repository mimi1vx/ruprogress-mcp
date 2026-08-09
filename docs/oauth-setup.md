# OAuth (bearer-token) setup

`REDMINE_AUTH_MODE=oauth` — each MCP client presents its own Redmine
Doorkeeper access token as `Authorization: Bearer`, and this server validates
it by RFC 7662 introspection before forwarding it upstream verbatim. Every
user acts as themselves, with their own token's permissions.

Ported from the upstream reference server's `docs/oauth-setup.md`
(`jztan/redmine-mcp-server`, branch `develop`, captured 2026-08-09), trimmed
to what `ruprogress-mcp` actually implements today.

## Status

**Live now (phase 6b1):** bearer extraction, RFC 7662 introspection with a
digest-keyed cache, the `401 WWW-Authenticate: Bearer` /
`503 Retry-After` challenge, and forwarding the validated token to Redmine.

**Not live yet:**

- RFC 9728/RFC 8414 discovery documents (`/.well-known/oauth-protected-resource`,
  `/.well-known/oauth-authorization-server`) — phase 6b2.
- `POST /revoke` and the introspection readiness probe behind `/readyz` — phase
  6b2.
- Per-tool scope enforcement (`REDMINE_OAUTH_SCOPE_ENFORCEMENT`,
  `REDMINE_MCP_SCOPES`) and `tools/list` filtering — phase 6b3. Every token
  that introspects as active and unexpired can call every tool today,
  regardless of its `scope`.
- `oauth-proxy` mode (this server acting as an authorization server with
  Dynamic Client Registration) — out of scope for v1.0 entirely.

`/readyz` reports `redmine: "not_probed"` in `oauth` mode until 6b2 wires the
introspection readiness probe — this is expected, not a bug.

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
# No token: 401 with a WWW-Authenticate challenge (the resource-metadata
# document it names is not served yet — that is 6b2).
curl -i http://127.0.0.1:8000/mcp

# A real token, obtained however your MCP client obtains one:
curl -i http://127.0.0.1:8000/mcp \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}'
```

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Every request is `401` with no `Authorization` header sent | Client isn't attaching a bearer token | Confirm the client actually obtained one from Redmine and is sending `Authorization: Bearer <token>` |
| `401` with `error="invalid_token"` | Token is inactive, revoked, or expired per introspection | Re-authorize; test directly with the Step 2c `curl` command against the real token |
| Every request is `503` | Introspection endpoint unreachable, misconfigured, or unmounted | Re-run Step 2c's verification `curl`; check `REDMINE_URL` and the introspection credentials |
| Server refuses to start naming `REDMINE_INTROSPECT_CLIENT_ID`/`_SECRET` | `oauth` mode requires them | Register the introspection client per Step 2 |
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

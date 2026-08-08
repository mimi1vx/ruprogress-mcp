# `legacy-per-user` authentication

`REDMINE_AUTH_MODE=legacy-per-user` — each HTTP request carries its own
Redmine API key instead of the server holding one shared credential. This is
the operator-facing summary of how the mode works and what it does and does
not protect against.

## How it works

Every request must carry the key it wants to act as, in the
`X-Redmine-API-Key` header — the same header `legacy` mode's single
credential is sent under, just supplied per request instead of once at
startup:

```
X-Redmine-API-Key: <the caller's Redmine API key>
```

`Authorization: Bearer`/`Basic` are never accepted or forwarded in this mode.
Bearer-token handling belongs to a future `oauth` mode, where a token is
*validated*, not blindly relayed — accepting one here would be a strictly
worse, unvalidated version of that mode.

The header is read once, at the start of the tool call, turned into a
`Credential::ApiKey` that lives only for that one request, and used to scope
exactly one Redmine client call. Nothing caches it, nothing reuses it for a
later request from the same connection, and no ambient/default credential
ever substitutes for a missing header — the request that didn't bring a key
gets nothing, not the last key seen.

A request that arrives with no header, an empty header, a header that isn't
visible ASCII, a header over 512 bytes, or the header sent more than once is
rejected before any request reaches Redmine, with a protocol-level error
naming the problem (never the header's value).

`initialize` and `tools/list` succeed with **no** credential header — the
credential check happens only at the point a tool actually needs Redmine, so
a client can discover the tool inventory before it has a key configured.
Nothing Redmine-shaped is reachable that way: tool descriptions, schemas, and
server metadata (not Redmine data) are all `tools/list` exposes.

## `REDMINE_PER_USER_TRUST_PROXY`

This mode refuses to start unless `REDMINE_PER_USER_TRUST_PROXY=true` is set.
Setting it is the operator's explicit attestation that:

- a TLS-terminating reverse proxy sits in front of this server, so
  `X-Redmine-API-Key` never crosses a network in cleartext, and
- that proxy does not forward a client-supplied `X-Forwarded-Proto` (or
  equivalent) that this server would otherwise have to trust blindly.

**This server cannot verify either claim.** There is no way, from inside the
process, to tell a TLS-terminated request forwarded correctly from a plaintext
one forwarded by a misconfigured or absent proxy. Setting the variable without
actually having that proxy in place means every request's API key crosses the
network in the clear. Startup logs an unmissable `WARN` naming this assumption
every time the mode is enabled — that log line is the whole enforcement
mechanism beyond the attestation itself.

This mode also refuses to start on the `stdio` transport (`Conflict`): there
is no per-request header on stdio to carry a credential over, so the mode is
meaningless there.

## `REDMINE_PER_USER_AUDIT_IDENTITY`

`REDMINE_PER_USER_AUDIT_IDENTITY=true` logs one line per tool call:

```
per-user request caller=<16 hex chars> request_id=<...>
```

`caller` is **not** the API key, not a Redmine login, and not reversible to
either: it is a `SipHash` over the key bytes, keyed by a random value chosen
once per process start (`std::collections::hash_map::RandomState`). The same
key produces the same fingerprint for the lifetime of one server process, so
requests from the same caller can be correlated within a run — but the
fingerprint changes on every restart and cannot be looked up against a
rainbow table, which is the privacy-correct shape for an audit breadcrumb.
This server never resolves the fingerprint back to a Redmine identity (that
would cost an extra `/users/current` call per tool call, doubling Redmine
traffic, for a log line).

## Threat model

- **In scope, mitigated structurally:** one caller's key can never scope
  another caller's request. `Scoped` (the only handle with a Redmine API
  surface) owns one credential for its entire lifetime and is built fresh
  per request; there is no shared, mutable, ambient credential slot for a
  bug to mis-point.
- **In scope, mitigated by testing:** the API key must never reach a log
  line, an error message, or a response body. Inbound HTTP headers are *not*
  marked `set_sensitive` by the underlying HTTP stack the way this server's
  own outbound `X-Redmine-API-Key` header is, so a stray `tracing::debug!(?
  parts)` anywhere on the request path would print it verbatim. This is
  covered by an end-to-end test that captures real log output at `TRACE`
  across success and failure paths and asserts on the captured bytes, not on
  the absence of a source-code pattern.
- **Accepted risk — `REDMINE_PER_USER_TRUST_PROXY` is unverifiable:** see
  above. Mitigated only by the explicit attestation requirement, the startup
  `WARN`, and this document.
- **Accepted risk — `tools/list`/`get_mcp_server_info` are unauthenticated:**
  an unauthenticated caller can learn the tool inventory, server version, and
  read-only/plugin flags. None of it is Redmine data, and gating discovery
  behind a credential would break clients that probe the tool list before
  they are configured with a key.
- **Accepted risk — `/files/{uuid}` has no credential check in this mode:**
  the UUID in a `get_redmine_attachment`-produced URL is an unguessable,
  TTL-bounded bearer capability, and the route exists specifically so a
  browser (or any non-MCP HTTP client) can fetch the bytes from a plain URL
  with no Redmine credential of its own. Binding that route to the fetching
  request's `X-Redmine-API-Key` would defeat that use case entirely, and
  disabling the route would break `get_redmine_attachment` outright. The
  route is marked with a `SECURITY:` comment at its definition
  (`src/transport/http.rs`) recording this trade-off.

## Out of scope

`oauth` (6b) and `oauth-proxy` (6c) are separate, unimplemented modes; nothing
here changes how they will work.

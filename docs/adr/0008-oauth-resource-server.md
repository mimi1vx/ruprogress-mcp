# ADR 0008: `oauth` as a resource server, not an authorization server

## Context

`REDMINE_AUTH_MODE=oauth` needs an MCP client to authenticate its user against
Redmine's Doorkeeper OAuth2 provider and present the resulting access token to
this server. The design space has three shapes: (1) hand-roll bearer
validation inside the existing per-request choke point (`RedmineMcp::scoped`,
the shape `legacy-per-user` uses), (2) mint our own tokens as a second
authorization server (`oauth-proxy`, explicitly out of scope for v1.0), or (3)
be a pure OAuth *resource server*: validate a token someone else issued, by
asking its issuer.

Two findings, verified against the vendored dependencies (`rmcp` 3.1.1,
`reqwest` 0.13.4), decided this ADR:

- **rmcp ships no server-side OAuth support.** Its `auth` feature and
  `transport::auth` module are an OAuth *client* (this process authenticating
  *to* another MCP server) — `AuthClient`/`OAuthState`/`AuthorizationManager`.
  Nothing in `streamable_http_server/` ever emits a `WWW-Authenticate` header.
  Enabling that feature would pull in `oauth2` for zero server-side benefit.
- **Doorkeeper issues opaque tokens with no published JWKS.** There is nothing
  to validate offline as a JWT. RFC 7662 introspection — a network call to
  Redmine per (uncached) token — is the only validation Redmine supports.

## Decisions

- **O1 — an axum middleware in front of the MCP route, not a check inside
  `scoped()`.** An MCP client discovers *that* it needs a token, and *where*
  to get one, from an HTTP `401` carrying `WWW-Authenticate: Bearer
  resource_metadata="…"`. A JSON-RPC-level error inside a `200` response (the
  `legacy-per-user` shape) is invisible to that machinery — the client never
  starts the OAuth flow. This means every MCP request, `initialize` included,
  needs a token in this mode; `legacy-per-user`'s decision to leave
  `initialize`/`tools/list` open does not carry over.
- **O2 — fail closed on a missing `AuthContext`.** The middleware inserts a
  validated `AuthContext` into the request extensions; `scoped()`'s OAuth arm
  reads it back out and returns `client.as_user_owned(Credential::Bearer(..))`,
  or an internal error — never a fallback — if the extension is absent. A
  future refactor that mounts the route without the middleware breaks loudly
  on every tool call instead of running unauthenticated.
- **O3 — introspection and revocation reuse `RedmineClient`, scoped to
  `Credential::Basic`, not a second `reqwest::Client`.** Doorkeeper is part of
  the Redmine deployment: same origin, same TLS/custom-CA/mTLS configuration,
  same timeouts, same connection pool. A second client would silently ignore
  `REDMINE_SSL_VERIFY` and the CA settings — the classic way a TLS bypass
  sneaks in.
- **O4 — the introspection cache is keyed by SHA-256 of the token, never the
  token itself**, with a positive TTL capped by the token's own `exp` and a
  short fixed TTL for a negative (`active:false`) result. A digest key means
  the cache cannot leak a plaintext token in a core dump or a stray `Debug`. A
  fingerprint collision in a log line (the shape `legacy-per-user`'s
  `KeyFingerprint` uses) is a cosmetic annoyance; a collision in this cache
  would serve one user's session to another, which is why that fingerprint is
  not reused here.
- **O7 — introspection *unavailability* is `503`, never `401`.** `401` tells
  the client "your token is bad, re-authorize"; sending it when this server
  (or Redmine) is broken would send every connected client through a
  pointless browser flow and hide an outage as a fleet-wide auth failure.

## Consequences

- **No audience binding.** Doorkeeper introspection returns no `aud`, so this
  server cannot verify a presented token was issued *for it* rather than for
  another OAuth client of the same Redmine. Any holder of any valid Redmine
  access token can drive this server as that user — the confused-deputy shape
  the MCP authorization spec warns about. Accepted: it is also the reference
  server's behaviour, unfixable without upstream Redmine changes, and bounded
  by the token granting no *more* than it already grants directly against
  Redmine's own REST API. Documented in `docs/oauth-setup.md`.
- **Cache staleness on revocation.** A token revoked at Redmine stays usable
  here for up to the cache's positive TTL (default 60s, capped further by the
  token's own `exp`). Mitigated by the low default and, once phase 6b2 lands
  `POST /revoke`, by that route purging the cache entry it revokes.
- **A new dependency, `sha2`,** on the auth path, for the sake of a cache key.
  Small, audited, RustCrypto-maintained; the alternative (no cache) doubles
  Redmine traffic per tool call, and a 64-bit keyed hash (as used for
  `legacy-per-user`'s audit fingerprint) is the wrong trade-off for a cache
  key rather than a log breadcrumb.
- **`legacy` and `legacy-per-user` are untouched.** The middleware is mounted
  only when `RedmineMcp::verifier()` returns `Some` (i.e. only in `oauth`
  mode), so neither existing mode's request shape, error shape, or tests
  change.

# ADR 0009: `oauth-proxy` as an authorization server — opaque tokens, memory-only state, no transparent refresh

## Context

`oauth` mode (ADR 0008) requires every MCP client to be hand-registered in
Redmine's admin panel with its exact redirect URI, since Doorkeeper has no
Dynamic Client Registration (DCR). `REDMINE_AUTH_MODE=oauth-proxy` closes
that gap: this server becomes an authorization server in its own right,
accepting RFC 7591 DCR and running its own authorization-code + PKCE flow
against `/authorize`/`/token`, which in turn drives a second,
server-internal authorization-code + PKCE flow against Redmine using one
operator-registered upstream OAuth application. This ADR records the three
decisions with the most durable consequences: what a client-facing token
*is*, where the resulting state lives, and why a token that expires stays
expired until the client asks for a new one.

## Decisions

- **Opaque, digest-keyed tokens — never a signed JWT.** A `rup_at_`/`rup_rt_`
  token is 256 bits of `OsRng` output, stored server-side keyed by its
  SHA-256 digest; the plaintext is never a map key and never appears in a
  `Debug`/core-dump-walkable structure. The reference implementation
  (FastMCP's `OAuthProxy`) signs a JWT and then immediately uses it as a
  database lookup key once verified — the signature is checked and then
  never consulted again. Skipping it costs nothing this design needs
  (there is no offline verifier anywhere else that would want to check the
  signature independently) and avoids a signing key, a rotation story, and
  an algorithm-confusion surface. `REDMINE_MCP_JWT_SIGNING_KEY`/`_FILE` is
  accepted and ignored, with one startup `WARN` naming it as unused, so the
  reference server's `.env` still boots here.
- **All proxy state is in-memory, bounded, and swept lazily on write — no
  persistence, no shared/multi-replica state.** The client registry, every
  in-flight authorization transaction, every authorization code, and every
  proxy access/refresh token live in `Mutex`-guarded maps inside this
  process. There is no `FASTMCP_HOME`-equivalent directory, no encryption
  key, no directory-permissions surface to get wrong. The cost is bounded
  and self-healing: a restart drops every registration and session, and an
  MCP client re-registers and re-authorizes driven by the same `401`
  challenge it already implements for the ordinary token-expiry case.
  Multiple replicas behind a round-robin load balancer are unsupported — a
  flow that starts `/authorize` on one instance and lands `/auth/callback`
  on another fails — so this mode is documented as single-instance (or
  sticky-session) only, in `docs/oauth-setup.md`.
- **No transparent refresh inside the request path — refreshing happens
  exactly once, in `/token`'s `refresh_token` grant.** FastMCP's proxy
  refreshes upstream transparently inside its bearer-verification path,
  because its JWT proxy tokens are signed with a lifetime independent of
  the upstream token and can outlive it. This design's proxy access token
  is deliberately capped at the upstream token's own remaining lifetime (it
  has no independent lifetime to claim), so there is nothing for a
  transparent refresh to buy: it would add a per-token async lock, a
  thundering-herd risk at every synchronized expiry, and latency on a
  request path that must stay fast, purely to solve a problem the
  `refresh_token` grant already solves within the protocol. A proxy access
  token whose upstream token has expired is `invalid_token`; the client
  uses its refresh token, exactly as OAuth 2.1 intends.

## Consequences

- **A client that never implements the refresh grant re-authorizes in a
  browser at every upstream expiry**, and a deployment whose upstream OAuth
  application has `use_refresh_token` disabled never gets a refresh token to
  use in the first place (logged once per process, not once per request).
  Both are documented in `docs/oauth-setup.md` rather than worked around,
  since a silent workaround (e.g. accepting a non-standard longer-lived
  token) would be a security regression, not a fix.
- **Refresh tokens rotate, and reuse of an already-rotated one kills the
  whole session and revokes it upstream immediately** — the standard cost of
  rotation with reuse detection. A client that legitimately retries a
  dropped refresh response will occasionally trip this and have to
  re-authorize; the alternative is accepting replay of a stolen refresh
  token, which is worse.
- **A restart, or a second replica, loses in-flight and issued state.**
  Accepted per the memory-only decision above; the store types are behind a
  narrow interface (`mint`/`resolve`/`take`/`redeem`/`retire`), so a future
  durable backend is a swap behind that interface rather than a rewrite.
- **`get_mcp_server_info` exposes `registered_clients`/`active_sessions`
  counts in this mode**, `null` elsewhere — enough for an operator to tell
  whether a restart dropped state, without exposing a client id, name, or
  subject that would let one caller learn who else is connected.

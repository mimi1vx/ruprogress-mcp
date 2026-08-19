# Troubleshooting

Operator-facing symptoms and their causes.

## Everything gets `429` behind my reverse proxy

**Symptom:** every client behind a load balancer, NAT gateway, or
TLS-terminating reverse proxy starts getting `429 {"error": "rate_limited"}`
from `/mcp`, even though no single client is sending anywhere near the
configured rate.

**Cause:** the rate limiter (`docs/configuration.md`'s "HTTP transport"
section) keys by the request's peer IP address only — it deliberately never
reads `X-Forwarded-For` or `X-Real-IP`, since either header is
attacker-controllable input a client could set itself to bypass the limiter
entirely. If every client's connection to this server arrives from one
address (a proxy's own egress IP), they all share one bucket.

**Fix:** rate-limit at the proxy, which can see real client addresses, rather
than raising this server's limits to compensate — a higher limit here would
also raise the ceiling for a single misbehaving client sharing that address.
If the deployment authenticates with `oauth`/`oauth-proxy` bearer tokens,
`/mcp` requests already key by token digest instead of IP when a token is
present, which sidesteps this for that route; the strict class
(`/register`&c.) and any legacy-mode request with no bearer token still key
by IP only.

# ADR 0006: Streamable HTTP transport

## Context

The stdio transport works, but every non-local client — MCP Inspector, hosted
assistants, anything behind a proxy — needs HTTP. rmcp 3.1.1 ships a
`StreamableHttpService` that implements the transport *and* most of its
security-relevant edge. An earlier design sketch assumed we would hand-roll
those checks; reading the crate source
(`rmcp-3.1.1/src/transport/streamable_http_server/tower.rs`, verified
2026-08-06) showed four of five prescriptions were already implemented
upstream.

## Findings against rmcp 3.1.1

Line references are to `tower.rs` at 3.1.1; re-verify them against a future
rmcp rather than trusting them.

1. **The DNS-rebinding guard is built in.** `allowed_hosts` defaults to
   `["localhost", "127.0.0.1", "::1"]` and rejects a mismatched `Host` with
   `403` (`validate_dns_rebinding_headers`, ~`:860`). `allowed_origins`
   defaults to `[]` (Origin validation disabled) and does RFC 6454
   `(scheme, host, port)` matching when non-empty. Malformed or non-UTF-8
   headers are `400`. Hand-rolling any of this would only add a second
   rejection path with different codes and different logs.
2. **The body cap is built in.** `max_request_body_bytes` (4 MiB default) is
   enforced *while streaming the body*, so `Content-Length`, chunked encoding,
   and HTTP version cannot be used to get around it. `RequestBodyLimitLayer`
   on top would be redundant and weaker.
3. **`Extension<T>` does exist**, at `rmcp::handler::server::common::Extension`
   (`impl<C, T> FromContextPart<C> for Extension<T>`, `common.rs:180`). This
   retracts a correction recorded during the stdio work; per-request HTTP
   headers are reachable from a tool handler, which makes per-user auth
   cheaper than previously assumed.
4. **A blanket timeout on the MCP route is wrong.** `tower_http`'s timeout
   covers the whole response *including the body*. Even with
   `json_response = true`, rmcp falls back to `text/event-stream` if a handler
   emits a notification before the final response, and a timeout would sever
   that stream mid-flight. Timeouts are scoped to the health routes only.
5. **`axum::Router::nest` can drop the `Host` header** hyper synthesises from
   an HTTP/2 `:authority`; rmcp works around it by falling back to
   `uri.authority()` (`parse_host_header`, ~`:850`). We use `nest_service`, as
   upstream's own tests do, and `tests/http_edge.rs` asserts `Host` validation
   still fires through the full axum layer stack.

Three further findings, each contradicting the design that preceded this ADR:

6. **`tower-http` 0.7 does not dedupe.** `reqwest` 0.13 depends on
   `tower-http ^0.6.8`, so requesting 0.7 in `[workspace.dependencies]`
   compiles two copies of the crate. The workspace is pinned to 0.6.
7. **`config.cancellation_token` is not a shutdown signal — it is a
   *request abort* signal.** rmcp races it against the handler's first message
   (`tower.rs:1228`) and converts a loser into a 500 "empty response". Handing
   it the same token as `axum::serve(...).with_graceful_shutdown(...)` would
   kill every in-flight tool call at the exact moment we claim to be draining.
   `serve()` therefore owns a second token, cancelled only *after* axum has
   finished draining. `tests/http_transport.rs`'s
   `a_shutdown_signal_lets_an_in_flight_tool_call_finish` fails with that 500
   if the two are ever merged again.
8. **A port-less `allowed_hosts` entry matches any port**
   (`host_is_allowed`, `tower.rs:759`). Adding both `host` and `host:port`, as
   originally specified, therefore makes the qualified entry — and
   `PUBLIC_PORT` — decorative. `PUBLIC_HOST` alone now adds the bare host, and
   `PUBLIC_PORT` replaces it with the pinned form.

## Decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | Default bind `127.0.0.1:8000`, diverging from upstream's `0.0.0.0` | One shared API key on every interface is one `docker run -p` from an unauthenticated Redmine proxy |
| D2 | Split `/livez` (process only) and `/readyz` (TTL-cached probe); `/health` aliases `/readyz` | A single `/health` invites wiring a dependency check to a `livenessProbe`, turning a Redmine blip into a restart storm |
| D3 | Stateless: `legacy_session_mode = false`, `json_response = true`, `NeverSessionManager` | No in-memory session state, plain JSON responses, no `Mcp-Session-Id` to expose through CORS, and edge tests writable with plain `reqwest` |
| D4 | `legacy-per-user` becomes *constructible* on HTTP; `scoped()` still returns "not yet implemented" | Transport and auth are separately reviewable; the interesting per-user tests are auth tests that happen to need HTTP |
| D5 | No rate limiting yet | Per-token keying only becomes meaningful once OAuth exists |
| D6 | A non-loopback bind with no Host policy is a **startup error** | The alternative is a `WARN` that scrolls past followed by a runtime `403` with no explanatory body |

### D3's consequences

Stateless mode is observable, so it is asserted rather than assumed:
`GET /mcp` and `DELETE /mcp` return `405` with `Allow: POST`
(`handle`, ~`:1512`), and no `Mcp-Session-Id` is issued. rmcp's own client
defaults to `allow_stateless: true`, so the e2e test needs no special config.

If a client turns up that hard-requires a session id, the fallback is
`session::local::LocalSessionManager` plus `with_legacy_session_mode(true)` —
a type alias and two builder calls, which is why `transport/http.rs` names the
session manager through a `type SessionManager` alias.

### D6's cost

This breaks `docker run -p 8000:8000` with `SERVER_HOST=0.0.0.0` and nothing
else. That previously worked by accident: the request arrives as
`Host: localhost:8000`, which the loopback entries cover. Operators now add
`PUBLIC_HOST=localhost`. The error message names both variables and the `*`
opt-out, because it is the thing an operator actually reads;
`docs/configuration.md` repeats it verbatim so a search lands on it.

The underlying reason is that in rmcp an *empty* `allowed_hosts` means **allow
every host** (`host_is_allowed` short-circuits to `true` on an empty slice,
`:755`), so a silently underivable allowlist is fail-open on exactly the
control that detects DNS rebinding — which CORS cannot help with, because
rebinding is same-origin from the browser's perspective.

Because "empty means allow-all" is the opposite of what the name suggests, the
emptiness invariant is enforced in three places rather than trusted:
`parse_csv` rejects a variable that is set but parses to nothing (`" , "` must
not degrade into "unset"), `parse_http` re-checks the derived list against the
explicit `*` opt-out, and a unit test enumerates inputs asserting no other one
can reach an empty list.

## Consequences

- Two shutdown paths now exist and behave differently: stdio aborts its
  serving task (ADR 0005's EINTR race), HTTP cancels a shared
  `CancellationToken` and drains for up to 10 s. They live in separate modules
  with the rationale attached to each.
- Three redaction surfaces now exist with three different rules
  (`--print-config`, `get_mcp_server_info`, `/readyz`). The table in
  `docs/configuration.md` exists so the next reader does not "unify" them.
- `/readyz` is an unauthenticated Redmine liveness oracle, and it sits
  *outside* the `Host` allowlist (rmcp enforces that only on its own service),
  so a DNS-rebound browser page can read it. Accepted: the strict three-key
  body keeps the disclosure to a single bit, and adding a second, hand-rolled
  Host check would be the duplication finding #1 argues against. Do not
  "improve" the body by adding the upstream error message.

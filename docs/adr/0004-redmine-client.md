# ADR 0004: Reject `redmine-api`, build `redmine-client`

## Context

`plans/phase-1-redmine-client.md` §1.0 requires a timeboxed (30 min) spike
scoring `redmine-api` 0.11.5 (crates.io, https://github.com/taladar/redmine-api,
last published 2026-06-08) against this project's load-bearing requirements
before committing to a hand-written client.

## Scoring

| Requirement | must? | `redmine-api` 0.11.5 |
|---|---|---|
| Per-request credential (not a client-per-user) | must | **No.** `Redmine::from_env(client)` / `Redmine::new(...)` binds one credential to the `Redmine` value at construction; the documented usage pattern is one `Redmine` per identity, not one pooled `reqwest::Client` shared across per-request credentials. |
| Caller-controlled pagination caps (page + total-item) | must | Partial. Offers single-page (`json_response_body_page`) and all-pages (`json_response_body_all_pages`) calls, but no `max_pages`/`max_items`/`max_response_bytes` caps or zero-progress termination guard. |
| Retry policy restricted to idempotent verbs | must | **No.** No retry logic found in `lib.rs`/`api.rs`; no `Retry-After` handling. |
| rustls, no OpenSSL; custom CA + mTLS | must | Partial. Examples use `reqwest::blocking::Client::builder().use_rustls_tls()`, so rustls is reachable, but there is no crate-level API for custom CA / mTLS — that's left entirely to the caller's own `reqwest::Client` construction. |
| Typed errors distinguishing 401/403/404/422/429 | must | **No.** `Error::HttpErrorResponse(reqwest::StatusCode)` is one untyped variant for all non-2xx statuses; 422's `{"errors": [...]}` body is not parsed into structured data. |
| Async (`tokio`) | must | Yes — both sync and async APIs, async is described as "significantly newer". |
| Maintained (release within ~12 months) | must | Yes — 0.11.5 published 2026-06-08, ~2 months before this ADR. |
| Plugin endpoints (Agile/Checklists/DMSF/…) | nice | No — stock REST API resources only. |

## Decision

**Reject** `redmine-api`. It fails three "must" requirements (per-request
credential scoping, typed status-code errors, retry policy), which are exactly
the load-bearing guarantees this project's parent plan (Risk 4: credential
plumbing; error-mapping tests in phase 1) depends on. Proceed with 1.1 onward:
build `redmine-client` in-house.

## Consequence

`redmine-client` is a new, from-scratch crate maintained by this project. This
is more work up front but avoids forking or vendoring `redmine-api` to retrofit
per-request credentials and typed errors — changes that would touch its core
`Redmine` struct and error type, i.e. most of the crate.

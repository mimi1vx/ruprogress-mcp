# redmine-client

A pure Redmine REST client for Rust. No MCP dependencies, no LLM-shaping of
responses — just typed requests and responses against Redmine's HTTP API.

```sh
cargo add redmine-client
```

## What it is not

This is not the `ruprogress-mcp` server (that lives one directory up). It
does not know about MCP tools, prompt shaping, or transports. It is the HTTP
layer the server is built on, usable on its own.

## What makes it different from a thin wrapper

- **Bounded pagination** — `Limits` caps how many items and pages an
  auto-paging call will fetch, so a runaway project can't exhaust memory.
- **Idempotent-only retry** — `RetryPolicy` only ever retries `GET`/`HEAD`;
  Redmine has no idempotency keys, so retrying a `POST` risks creating a
  duplicate issue.
- **`rustls`-only TLS** — no OpenSSL in the dependency tree.
- **Path-segment validation** — identifiers like project slugs and wiki
  titles are validated before they ever reach a URL, so a malformed value
  fails at the call site instead of producing a broken request.
- **Same-origin credentials** — a credential is never sent anywhere but the
  configured Redmine origin. Off-origin redirects are stopped rather than
  followed, and an attachment `content_url` pointing elsewhere (or carrying
  embedded userinfo) is refused with `Error::ForeignOrigin` before the
  request is built.

## Example

```rust
use redmine_client::{Credential, RedmineClientBuilder};
use secrecy::SecretString;
use url::Url;

async fn whoami() -> Result<(), Box<dyn std::error::Error>> {
    let base_url = Url::parse("https://redmine.example.com/")?;
    let client = RedmineClientBuilder::new(base_url)
        .credential(Credential::ApiKey(SecretString::from("api-key")))
        .build()?;

    let me = client.as_default()?.current_user().await?;
    println!("{:?}", me.login);
    Ok(())
}
```

## One client, many callers

`RedmineClient` holds the connection pool and the configuration; it cannot
issue a request on its own. Every call goes through a `Scoped` handle naming
exactly one credential, so a credential can never be picked up ambiently:

| Handle | Credential |
|---|---|
| `as_default()` | The one configured on the builder. `Error::Config` if there is none. |
| `as_user(&credential)` | A caller's own, cloned. |
| `as_user_owned(credential)` | The same, without the clone — for a credential built fresh per request. |

`Credential` is `ApiKey` (`X-Redmine-API-Key`), `Basic`, or `Bearer`. All
three wrap a `secrecy::SecretString` and are applied as a header marked
sensitive, so `http`'s own `Debug` and `tower-http`'s trace layer redact them.

A server fronting many users therefore builds one client and scopes it per
request:

```rust
use redmine_client::model::user::User;
use redmine_client::{Credential, RedmineClient};
use secrecy::SecretString;

async fn as_caller(client: &RedmineClient, api_key: &str) -> redmine_client::Result<User> {
    let caller = Credential::ApiKey(SecretString::from(api_key.to_owned()));
    client.as_user_owned(caller).current_user().await
}
```

## Bounds and retries

Both are caller-owned and set on the builder:

```rust
use std::time::Duration;

use redmine_client::{Limits, RedmineClient, RedmineClientBuilder, RetryPolicy};
use url::Url;

fn tuned(base_url: Url) -> redmine_client::Result<RedmineClient> {
    RedmineClientBuilder::new(base_url)
        .limits(Limits { max_items: 500, ..Limits::default() })
        .retry_policy(RetryPolicy { max_retries: 5, ..RetryPolicy::default() })
        .timeout(Duration::from_secs(30))
        .build()
}
```

| `Limits` | Default | |
|---|---|---|
| `page_size` | 100 | Redmine's own server-side maximum. |
| `max_pages` | 20 | Pages an auto-paging call will walk. |
| `max_items` | 2 000 | Items collected across all pages. |
| `max_response_bytes` | 32 MiB | Any single response body. |

Hitting an item or page cap is not an error: the walk stops and `Page`
reports `truncated: true` alongside Redmine's `total_count`, so a large
project stays browsable. `max_response_bytes` is the one that does fail —
a declared `Content-Length` above it is rejected without reading a byte, and
a chunked body is aborted mid-stream the moment it crosses the limit, so the
client never buffers more than the limit. `download_attachment` is the
deliberate exception: it hands back a `Stream` and leaves the byte cap to the
caller, which keeps this crate filesystem-free.

`RetryPolicy` defaults to 3 retries with full-jitter exponential backoff
(200 ms base, 5 s ceiling), honours `Retry-After`, and is bounded by the
client's total timeout — retries never extend the caller's deadline.

## Beyond CRUD

Alongside issues, projects, versions, members, time entries, wiki pages,
attachments, and the plugin models (checklists, agile, products, CRM contacts,
DMSF documents, tags), the client speaks the Doorkeeper endpoints a
resource server needs: `introspect_token` (RFC 7662), `revoke_token`
(RFC 7009), `exchange_authorization_code`, and `refresh_access_token`.

Errors are one `#[non_exhaustive]` enum — `Unauthorized`, `Forbidden`,
`NotFound`, `RateLimited` (with Redmine's `Retry-After`), `Status`,
`Transport`, `Decode`, `Config`, `LimitExceeded`, `OAuth`, `ForeignOrigin` —
so a caller can match on the case it handles without stringly-typed checks.
Every response struct is `#[non_exhaustive]` too: it can only be obtained by
deserializing a real Redmine response, never built as a struct literal by
downstream code — including this crate's own tests.

## Requirements

Rust 1.96 (edition 2024). MIT licensed. Full API documentation on
[docs.rs](https://docs.rs/redmine-client).

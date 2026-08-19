# redmine-client

A pure Redmine REST client for Rust. No MCP dependencies, no LLM-shaping of
responses — just typed requests and responses against Redmine's HTTP API.

## What it is not

This is not the `ruprogress-mcp` server (that lives one directory up). It
does not know about MCP tools, prompt shaping, or transports. It is the HTTP
layer the server is built on, usable on its own.

## What makes it different from a thin wrapper

- **Bounded pagination** — [`Limits`] caps how many items and pages an
  auto-paging call will fetch, so a runaway project can't exhaust memory.
- **Idempotent-only retry** — [`RetryPolicy`] only ever retries `GET`/`HEAD`;
  Redmine has no idempotency keys, so retrying a `POST` risks creating a
  duplicate issue.
- **`rustls`-only TLS** — no OpenSSL in the dependency tree.
- **Path-segment validation** — identifiers like project slugs and wiki
  titles are validated before they ever reach a URL, so a malformed value
  fails at the call site instead of producing a broken request.

## Example

```rust,no_run
use redmine_client::{Credential, RedmineClientBuilder};
use secrecy::SecretString;
use url::Url;

# async fn run() -> redmine_client::Result<()> {
let base_url = Url::parse("https://redmine.example.com/")?;
let client = RedmineClientBuilder::new(base_url)
    .credential(Credential::ApiKey(SecretString::from("api-key")))
    .build()?;

let me = client.as_default()?.current_user().await?;
println!("{:?}", me.login);
# Ok(())
# }
```

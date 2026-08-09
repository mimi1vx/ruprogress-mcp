//! The Redmine HTTP client: builder, credential scoping, and request core.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_core::Stream;
use futures_util::TryStreamExt as _;
use secrecy::{ExposeSecret as _, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::auth::Credential;
use crate::error::Error;
use crate::ids::{
    AttachmentId, IssueCategoryId, IssueId, JournalId, MembershipId, ProjectIdent, RelationId,
    TimeEntryId, UserId, VersionId, WikiTitle,
};
use crate::model::{
    BareCollection, Collection, attachment, custom_field, enumeration, introspection, issue,
    issue_category, issue_status, journal, membership, project, query, relation, role, search,
    time_entry, tracker, upload, user, version, wiki,
};
use crate::page::{Limits, Page};
use crate::retry::{self, RetryPolicy};

/// Query parameters for a single request, kept in a `BTreeMap` for
/// deterministic ordering (stable wiremock matchers, readable diffs).
#[derive(Debug, Default, Clone)]
pub struct Query(BTreeMap<String, String>);

impl Query {
    /// Set a query parameter, overwriting any previous value for `key`.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.0.insert(key.into(), value.into());
        self
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Builder for [`RedmineClient`].
pub struct RedmineClientBuilder {
    base_url: Url,
    credential: Option<Credential>,
    timeout: Duration,
    connect_timeout: Duration,
    user_agent: Option<String>,
    root_cert_pems: Vec<Vec<u8>>,
    identity_pem: Option<Vec<u8>>,
    danger_accept_invalid_certs: bool,
    limits: Limits,
    retry_policy: RetryPolicy,
}

impl core::fmt::Debug for RedmineClientBuilder {
    // Manual: `root_cert_pems`/`identity_pem` hold raw PEM bytes (the latter
    // a private key for mTLS) that must never appear in a `Debug` dump.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RedmineClientBuilder")
            .field("base_url", &self.base_url)
            .field("credential", &self.credential)
            .field("timeout", &self.timeout)
            .field("connect_timeout", &self.connect_timeout)
            .field("user_agent", &self.user_agent)
            .field(
                "root_cert_pems",
                &format!("<{} certificate(s)>", self.root_cert_pems.len()),
            )
            .field(
                "identity_pem",
                &self.identity_pem.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "danger_accept_invalid_certs",
                &self.danger_accept_invalid_certs,
            )
            .field("limits", &self.limits)
            .field("retry_policy", &self.retry_policy)
            .finish()
    }
}

impl RedmineClientBuilder {
    /// Start building a client against `base_url` (e.g.
    /// `https://redmine.example.com/`).
    #[must_use]
    pub fn new(base_url: Url) -> Self {
        Self {
            base_url,
            credential: None,
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            user_agent: None,
            root_cert_pems: Vec::new(),
            identity_pem: None,
            danger_accept_invalid_certs: false,
            limits: Limits::default(),
            retry_policy: RetryPolicy::default(),
        }
    }

    /// Set the credential used when no per-request credential is supplied
    /// via [`RedmineClient::as_user`] — i.e. enables [`RedmineClient::as_default`].
    #[must_use]
    pub fn credential(mut self, c: Credential) -> Self {
        self.credential = Some(c);
        self
    }

    /// Total budget for a single logical request, including retries and
    /// backoff. Default 30s.
    #[must_use]
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    /// TCP+TLS connect timeout for a single attempt. Default 10s.
    #[must_use]
    pub fn connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = d;
        self
    }

    /// Override the `User-Agent` header.
    #[must_use]
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = Some(ua.into());
        self
    }

    /// Trust an additional CA certificate (PEM-encoded), merged with the
    /// platform's trust store. A malformed certificate is rejected here,
    /// at build time — never silently falls back to the system roots alone.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if `pem` does not parse as a certificate.
    pub fn add_root_certificate_pem(mut self, pem: &[u8]) -> crate::Result<Self> {
        // Validate eagerly so a bad cert fails at the call site, not deep
        // inside `.build()`. `from_pem_bundle` (unlike `from_pem`) actually
        // parses PEM blocks up front, which also lets us catch the case
        // `from_pem` alone would miss: input with no PEM markers at all
        // parses to zero certificates without ever erroring.
        let certs = reqwest::Certificate::from_pem_bundle(pem).map_err(|e| Error::Config {
            reason: format!("invalid root certificate PEM: {e}"),
        })?;
        if certs.is_empty() {
            return Err(Error::Config {
                reason: "no certificates found in the provided PEM data".to_string(),
            });
        }
        self.root_cert_pems.push(pem.to_vec());
        Ok(self)
    }

    /// Configure mTLS: a client certificate + private key in one PEM.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if `pem` does not parse as a certificate
    /// and key pair.
    pub fn client_identity_pem(mut self, pem: &[u8]) -> crate::Result<Self> {
        reqwest::Identity::from_pem(pem).map_err(|e| Error::Config {
            reason: format!("invalid client identity PEM: {e}"),
        })?;
        self.identity_pem = Some(pem.to_vec());
        Ok(self)
    }

    /// Disable TLS certificate verification. **Dangerous**: only ever for a
    /// pinned test/staging instance. Logs a `WARN` unconditionally when
    /// `build()` runs with this set to `true`.
    #[must_use]
    pub fn danger_accept_invalid_certs(mut self, yes: bool) -> Self {
        self.danger_accept_invalid_certs = yes;
        self
    }

    /// Set pagination and response-size caps.
    #[must_use]
    pub fn limits(mut self, l: Limits) -> Self {
        self.limits = l;
        self
    }

    /// Set the retry policy for idempotent (`GET`/`HEAD`) requests.
    #[must_use]
    pub fn retry_policy(mut self, r: RetryPolicy) -> Self {
        self.retry_policy = r;
        self
    }

    /// Finish building the client.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the underlying `reqwest::Client` fails
    /// to build (e.g. a certificate rejected at this late stage by the TLS
    /// backend despite passing the eager parse in `add_root_certificate_pem`).
    pub fn build(self) -> crate::Result<RedmineClient> {
        if self.danger_accept_invalid_certs {
            tracing::warn!("TLS certificate verification is DISABLED for this RedmineClient");
        }

        let mut http = reqwest::Client::builder()
            .timeout(self.timeout)
            .connect_timeout(self.connect_timeout)
            .tls_danger_accept_invalid_certs(self.danger_accept_invalid_certs);

        if let Some(ua) = &self.user_agent {
            http = http.user_agent(ua);
        }

        if !self.root_cert_pems.is_empty() {
            let certs = self
                .root_cert_pems
                .iter()
                .map(|pem| {
                    reqwest::Certificate::from_pem(pem).map_err(|e| Error::Config {
                        reason: format!("invalid root certificate PEM: {e}"),
                    })
                })
                .collect::<crate::Result<Vec<_>>>()?;
            http = http.tls_certs_merge(certs);
        }

        if let Some(pem) = &self.identity_pem {
            let identity = reqwest::Identity::from_pem(pem).map_err(|e| Error::Config {
                reason: format!("invalid client identity PEM: {e}"),
            })?;
            http = http.identity(identity);
        }

        let http = http.build().map_err(|e| Error::Config {
            reason: format!("failed to build HTTP client: {}", e.without_url()),
        })?;

        let mut base = self.base_url;
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }

        Ok(RedmineClient {
            inner: Arc::new(Inner {
                http,
                base,
                default_credential: self.credential,
                limits: self.limits,
                retry_policy: self.retry_policy,
                total_timeout: self.timeout,
            }),
        })
    }
}

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    base: Url,
    default_credential: Option<Credential>,
    limits: Limits,
    retry_policy: RetryPolicy,
    total_timeout: Duration,
}

/// A Redmine client: one pooled `reqwest::Client` shared across every
/// credential in the process. Cheap to clone (an `Arc` bump).
#[derive(Debug, Clone)]
pub struct RedmineClient {
    inner: Arc<Inner>,
}

impl RedmineClient {
    /// Scope requests to `credential`. This is the only way to reach the API
    /// surface: a credential must always be named explicitly, so one can
    /// never be picked up ambiently. Clones `credential` into the returned
    /// `Scoped` (see [`Self::as_user_owned`] to avoid that clone when the
    /// caller already owns the credential outright).
    #[must_use]
    pub fn as_user(&self, credential: &Credential) -> Scoped<'_> {
        Scoped {
            inner: &self.inner,
            credential: credential.clone(),
        }
    }

    /// Like [`Self::as_user`], but takes ownership of `credential` instead of
    /// cloning it — for callers (e.g. a per-request credential built fresh
    /// from an inbound header) that would otherwise clone once to construct
    /// the credential and again to hand it to `as_user`.
    #[must_use]
    pub fn as_user_owned(&self, credential: Credential) -> Scoped<'_> {
        Scoped {
            inner: &self.inner,
            credential,
        }
    }

    /// Scope requests to the credential configured on the builder.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if no default credential was configured
    /// (i.e. this client is in per-user mode).
    pub fn as_default(&self) -> crate::Result<Scoped<'_>> {
        let credential = self
            .inner
            .default_credential
            .as_ref()
            .ok_or_else(|| Error::Config {
                reason: "no default credential configured; use as_user(...) instead".to_string(),
            })?;
        Ok(Scoped {
            inner: &self.inner,
            credential: credential.clone(),
        })
    }
}

/// A [`RedmineClient`] scoped to one credential. The only handle that can
/// perform a request. Owns its `Credential` (a cheap ~40-byte clone from the
/// caller) rather than borrowing it, so a per-request credential built
/// locally (e.g. from an inbound header) can be scoped without a lifetime
/// tying it to the caller's stack frame.
#[derive(Debug)]
pub struct Scoped<'a> {
    inner: &'a Inner,
    credential: Credential,
}

impl Scoped<'_> {
    /// Resolve `path` (relative to the API base) to a full URL.
    ///
    /// This is a security boundary, not a convenience: `path` must not start
    /// with `/` (that would discard the base's own sub-path, e.g. `/redmine`)
    /// and must not contain `..`, `//`, or a control character.
    fn endpoint(&self, path: &str) -> crate::Result<Url> {
        if path.starts_with('/') {
            return Err(Error::Config {
                reason: format!(
                    "path must be relative to the API base, got absolute path {path:?}"
                ),
            });
        }
        if path.contains("..") || path.contains("//") {
            return Err(Error::Config {
                reason: format!("path must not contain '..' or '//': {path:?}"),
            });
        }
        if path.chars().any(char::is_control) {
            return Err(Error::Config {
                reason: "path must not contain control characters".to_string(),
            });
        }
        self.inner.base.join(path).map_err(|e| Error::Config {
            reason: format!("invalid path {path:?}: {e}"),
        })
    }

    fn build_url(&self, path: &str, query: Option<&Query>) -> crate::Result<Url> {
        let mut url = self.endpoint(path)?;
        if let Some(q) = query
            && !q.is_empty()
        {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in &q.0 {
                pairs.append_pair(k, v);
            }
        }
        Ok(url)
    }

    /// Send `template`, retrying per [`RetryPolicy`] when `method` is
    /// retryable and the failure looks transient. The whole retry budget —
    /// attempts and backoff sleeps — is bounded by the client's configured
    /// total timeout; retries never extend the caller's deadline.
    async fn send_with_retry(
        &self,
        method: &http::Method,
        template: &reqwest::RequestBuilder,
    ) -> crate::Result<reqwest::Response> {
        let policy = self.inner.retry_policy;
        let now = Instant::now();
        let deadline = now.checked_add(self.inner.total_timeout).unwrap_or(now);
        let mut attempt: u32 = 0;

        loop {
            let req = template.try_clone().ok_or_else(|| Error::Config {
                reason: "request body cannot be cloned for retry".to_string(),
            })?;

            let outcome = req.send().await;
            let err = match outcome {
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp) => {
                    let status = resp.status();
                    let retry_after = retry::retry_after(resp.headers())
                        .map(|d| retry::clamp_retry_after(d, policy.max_backoff));
                    let body = resp.bytes().await.map_err(Error::transport)?;
                    if body.len() as u64 > self.inner.limits.max_response_bytes {
                        return Err(Error::LimitExceeded {
                            what: "response bytes",
                            limit: self.inner.limits.max_response_bytes,
                            actual: body.len() as u64,
                        });
                    }
                    crate::error::from_status(status, &body, retry_after)
                }
                Err(e) => Error::transport(e),
            };

            let now = Instant::now();
            let can_retry = retry::method_is_retryable(method)
                && err.is_retryable()
                && attempt < policy.max_retries
                && now < deadline;

            if !can_retry {
                return Err(err);
            }

            let remaining = deadline.saturating_duration_since(now);
            let backoff = match &err {
                Error::RateLimited {
                    retry_after: Some(d),
                } => *d,
                _ => retry::backoff_duration(&policy, attempt),
            }
            .min(remaining);

            tokio::time::sleep(backoff).await;
            attempt = attempt.saturating_add(1);
        }
    }

    async fn read_json<T: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
        context: &'static str,
    ) -> crate::Result<T> {
        let bytes = resp.bytes().await.map_err(Error::transport)?;
        if bytes.len() as u64 > self.inner.limits.max_response_bytes {
            return Err(Error::LimitExceeded {
                what: "response bytes",
                limit: self.inner.limits.max_response_bytes,
                actual: bytes.len() as u64,
            });
        }
        serde_json::from_slice(&bytes).map_err(|source| Error::Decode { context, source })
    }

    pub(crate) async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        q: &Query,
    ) -> crate::Result<T> {
        let url = self.build_url(path, Some(q))?;
        let template = self.credential.apply(self.inner.http.get(url));
        let resp = self.send_with_retry(&http::Method::GET, &template).await?;
        self.read_json(resp, "response").await
    }

    pub(crate) async fn post_json<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> crate::Result<T> {
        let url = self.build_url(path, None)?;
        let template = self.credential.apply(self.inner.http.post(url)).json(body);
        let resp = self.send_with_retry(&http::Method::POST, &template).await?;
        self.read_json(resp, "response").await
    }

    /// Send `form` as `application/x-www-form-urlencoded`, returning the raw
    /// response for the caller to decode (or discard). Not covered by the
    /// retry policy for a different reason than the JSON POST helpers: it
    /// exists only for the OAuth introspection/revocation endpoints, which
    /// this crate's retry rule (idempotent verbs only) already excludes as a
    /// `POST`.
    async fn post_form<B: Serialize>(
        &self,
        path: &str,
        form: &B,
    ) -> crate::Result<reqwest::Response> {
        let url = self.build_url(path, None)?;
        let template = self.credential.apply(self.inner.http.post(url)).form(form);
        self.send_with_retry(&http::Method::POST, &template).await
    }

    /// Like [`Self::post_form`], decoding the response body as JSON.
    async fn post_form_json<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        form: &B,
    ) -> crate::Result<T> {
        let resp = self.post_form(path, form).await?;
        self.read_json(resp, "response").await
    }

    pub(crate) async fn put_json<B: Serialize>(&self, path: &str, body: &B) -> crate::Result<()> {
        let url = self.build_url(path, None)?;
        let template = self.credential.apply(self.inner.http.put(url)).json(body);
        self.send_with_retry(&http::Method::PUT, &template).await?;
        Ok(())
    }

    pub(crate) async fn delete(&self, path: &str) -> crate::Result<()> {
        let url = self.build_url(path, None)?;
        let template = self.credential.apply(self.inner.http.delete(url));
        self.send_with_retry(&http::Method::DELETE, &template)
            .await?;
        Ok(())
    }

    /// Like [`Self::delete`], but with query parameters (e.g.
    /// `?reassign_to_id=...`).
    pub(crate) async fn delete_with_query(&self, path: &str, q: &Query) -> crate::Result<()> {
        let url = self.build_url(path, Some(q))?;
        let template = self.credential.apply(self.inner.http.delete(url));
        self.send_with_retry(&http::Method::DELETE, &template)
            .await?;
        Ok(())
    }

    /// Like [`Self::post_json`], for endpoints that answer `204 No Content`
    /// with no body to decode (e.g. `POST /issues/{id}/watchers.json`).
    pub(crate) async fn post_json_no_content<B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> crate::Result<()> {
        let url = self.build_url(path, None)?;
        let template = self.credential.apply(self.inner.http.post(url)).json(body);
        self.send_with_retry(&http::Method::POST, &template).await?;
        Ok(())
    }

    /// Like [`Self::post_json`], but for an endpoint that wants a raw byte
    /// body with an explicit `Content-Type` rather than a JSON payload
    /// (`POST /uploads.json`, per `AttachmentsController#upload`, which
    /// 406s any request whose `Content-Type` isn't exactly
    /// `application/octet-stream`).
    pub(crate) async fn post_bytes<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &Query,
        content_type: &str,
        body: Bytes,
    ) -> crate::Result<T> {
        let url = self.build_url(path, Some(query))?;
        let template = self
            .credential
            .apply(self.inner.http.post(url))
            .header(http::header::CONTENT_TYPE, content_type)
            .body(body);
        let resp = self.send_with_retry(&http::Method::POST, &template).await?;
        self.read_json(resp, "response").await
    }

    /// Fetch an attachment's content from `content_url` (as returned by
    /// [`Self::get_attachment`]/[`Self::list_project_files`] —  an
    /// **absolute** URL, unlike every other method on this type, which
    /// resolves a path relative to the API base via [`Self::endpoint`]).
    /// Returns the response headers plus a byte stream of the body,
    /// deliberately bypassing [`Self::read_json`]/[`Limits::max_response_bytes`]:
    /// an attachment can be up to `ATTACHMENT_MAX_DOWNLOAD_BYTES` (200 MB by
    /// default in `ruprogress-mcp`), far past what should ever be buffered
    /// into one `Vec`. The caller (`ruprogress-mcp`'s attachment store) owns
    /// enforcing a byte cap mid-stream and writing to disk; this crate stays
    /// filesystem-free.
    ///
    /// `content_url` is used as given — it is expected to be a value
    /// Redmine itself returned from an earlier call in the same scope, not
    /// arbitrary caller-supplied input.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if `content_url` does not parse. Returns a
    /// transport or status error if the request itself fails; errors
    /// surfaced by the stream (a connection drop mid-body) are yielded as
    /// stream items, not from this method.
    pub async fn download_attachment(
        &self,
        content_url: &str,
    ) -> crate::Result<(
        http::HeaderMap,
        impl Stream<Item = crate::Result<Bytes>> + Send + use<>,
    )> {
        let url: Url = content_url.parse().map_err(|e| Error::Config {
            reason: format!("invalid attachment content_url: {e}"),
        })?;
        let template = self.credential.apply(self.inner.http.get(url));
        let resp = self.send_with_retry(&http::Method::GET, &template).await?;
        let headers = resp.headers().clone();
        let stream = resp.bytes_stream().map_err(Error::transport);
        Ok((headers, stream))
    }

    /// Walk every page of a Redmine collection endpoint, subject to
    /// [`Limits`]. Terminates when (1) all items have been collected, (2) a
    /// cap is hit — returned as `truncated = true`, not an error, so a big
    /// project stays browsable — or (3) the server makes zero progress
    /// (an empty page, or an `offset` that doesn't advance), which would
    /// otherwise loop forever against a misbehaving or proxied Redmine.
    pub(crate) async fn fetch_all<W: Collection>(
        &self,
        path: &str,
        query: &Query,
    ) -> crate::Result<Page<W::Item>> {
        let limits = self.inner.limits;
        let mut items: Vec<W::Item> = Vec::new();
        let mut offset: u64 = 0;
        let mut total_count: u64 = 0;
        let truncated: bool;
        let mut limit_used = limits.page_size;

        let mut pages_fetched: u32 = 0;
        loop {
            if pages_fetched >= limits.max_pages {
                truncated = offset < total_count;
                break;
            }

            let mut q = query.clone();
            q.insert("limit", limits.page_size.to_string());
            q.insert("offset", offset.to_string());

            let envelope: W = self.get_json(path, &q).await?;
            total_count = envelope.total_count();
            limit_used = envelope.limit();
            let page_offset = envelope.offset();
            let page_items = envelope.into_items();
            let got = page_items.len();
            pages_fetched = pages_fetched.saturating_add(1);

            if got == 0 {
                truncated = offset < total_count;
                break;
            }
            items.extend(page_items);

            let Some(new_offset) = page_offset.checked_add(u64::try_from(got).unwrap_or(u64::MAX))
            else {
                truncated = true;
                break;
            };
            if new_offset <= offset {
                // Zero progress: the server reported an offset that didn't
                // advance. Stop rather than loop forever.
                truncated = true;
                break;
            }
            offset = new_offset;

            if offset >= total_count {
                truncated = false;
                break;
            }
            if items.len() >= limits.max_items {
                truncated = true;
                break;
            }
        }

        Ok(Page {
            items,
            total_count,
            offset,
            limit: limit_used,
            truncated,
        })
    }

    /// Fetch a Redmine collection endpoint that carries **no** pagination
    /// envelope (e.g. `{"trackers": [...]}`), sending no `limit`/`offset`
    /// query parameters. Errors loudly if the response turns out to carry a
    /// `total_count` field: that means the endpoint is actually paginated,
    /// and silently returning only its first page would be worse than
    /// failing outright.
    pub(crate) async fn get_collection<W: BareCollection>(
        &self,
        path: &str,
        query: &Query,
    ) -> crate::Result<Vec<W::Item>> {
        let value: serde_json::Value = self.get_json(path, query).await?;
        if value.get("total_count").is_some() {
            use serde::de::Error as _;
            return Err(Error::Decode {
                context: "un-paginated collection response",
                source: serde_json::Error::custom(
                    "response carries a total_count field; this endpoint is paginated, use fetch_page/fetch_all instead",
                ),
            });
        }
        let envelope: W = serde_json::from_value(value).map_err(|source| Error::Decode {
            context: "un-paginated collection response",
            source,
        })?;
        Ok(envelope.into_items())
    }

    /// Fetch exactly one page of a Redmine collection endpoint that exposes
    /// `limit`/`offset` as tool-level parameters. Sends exactly the
    /// `limit`/`offset` given and never follows on to a second page — unlike
    /// [`Scoped::fetch_all`], which auto-pages endpoints with no exposed
    /// `limit`/`offset` parameter.
    pub(crate) async fn fetch_page<W: Collection>(
        &self,
        path: &str,
        query: &Query,
        limit: u32,
        offset: u64,
    ) -> crate::Result<Page<W::Item>> {
        let mut q = query.clone();
        q.insert("limit", limit.to_string());
        q.insert("offset", offset.to_string());
        let envelope: W = self.get_json(path, &q).await?;
        let total_count = envelope.total_count();
        let offset = envelope.offset();
        let limit = envelope.limit();
        Ok(Page {
            items: envelope.into_items(),
            total_count,
            offset,
            limit,
            // A single explicit page is never truncated by our own limits:
            // nothing here decided to stop early. Whether more data exists
            // beyond this page is pagination metadata for the caller to
            // derive from total_count/limit/offset, not this flag.
            truncated: false,
        })
    }
}

// --- Issue, project, and time-entry API surface ---

impl Scoped<'_> {
    /// `GET /my/account.json` — the currently authenticated user. Works for
    /// any credential; **not** `/users/current.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn current_user(&self) -> crate::Result<user::User> {
        let env: user::UserEnvelope = self.get_json("my/account.json", &Query::default()).await?;
        Ok(env.user)
    }

    /// `POST /oauth/introspect` (RFC 7662). The scoping credential must be
    /// `Credential::Basic { user: client_id, pass: client_secret }` for the
    /// confidential OAuth client registered for token introspection — this
    /// is one of the two methods on this type where the scoping credential
    /// is not an end-user identity.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unauthorized`]/[`Error::Forbidden`] if the scoping
    /// client credentials are rejected, [`Error::NotFound`] if the
    /// introspection route is unmounted (Redmine's `allow_token_introspection`
    /// defaults to `false`), or a transport/decode error otherwise. Never
    /// itself an error for an inactive/expired/unknown token — that is
    /// `Introspection::active`.
    pub async fn introspect_token(
        &self,
        token: &SecretString,
    ) -> crate::Result<introspection::Introspection> {
        #[derive(Serialize)]
        struct Form<'a> {
            token: &'a str,
            token_type_hint: &'a str,
        }
        self.post_form_json(
            "oauth/introspect",
            &Form {
                token: token.expose_secret(),
                token_type_hint: "access_token",
            },
        )
        .await
    }

    /// `POST /oauth/revoke` (RFC 7009). Same scoping-credential requirement
    /// as [`Self::introspect_token`]. Per RFC 7009 a `200` is success whether
    /// or not `token` was a token this client ever issued — revoking an
    /// unknown token is not an error.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unauthorized`]/[`Error::Forbidden`] if the scoping
    /// client credentials are rejected, or a transport error otherwise.
    pub async fn revoke_token(
        &self,
        token: &SecretString,
        hint: Option<&str>,
    ) -> crate::Result<()> {
        #[derive(Serialize)]
        struct Form<'a> {
            token: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            token_type_hint: Option<&'a str>,
        }
        self.post_form(
            "oauth/revoke",
            &Form {
                token: token.expose_secret(),
                token_type_hint: hint,
            },
        )
        .await?;
        Ok(())
    }

    /// `GET /projects.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn list_projects(
        &self,
        q: &project::ProjectQuery,
    ) -> crate::Result<Page<project::Project>> {
        self.fetch_all::<project::ProjectsEnvelope>("projects.json", &q.to_query())
            .await
    }

    /// `GET /projects/{id}.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn get_project(
        &self,
        id: &ProjectIdent,
        inc: &[project::ProjectInclude],
    ) -> crate::Result<project::Project> {
        let mut q = Query::default();
        if let Some(include) = project::includes_to_query_value(inc) {
            q.insert("include", include);
        }
        let env: project::ProjectEnvelope =
            self.get_json(&format!("projects/{id}.json"), &q).await?;
        Ok(env.project)
    }

    /// `GET /issues.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn list_issues(&self, q: &issue::IssueQuery) -> crate::Result<Page<issue::Issue>> {
        self.fetch_all::<issue::IssuesEnvelope>("issues.json", &q.to_query())
            .await
    }

    /// `GET /issues.json`, a single explicit page — unlike [`Self::list_issues`],
    /// never auto-pages. Used by tools that expose `limit`/`offset` directly
    /// (and by `summarize_project_status`'s count-only and sample-breakdown
    /// calls, which only ever want one bounded page).
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn list_issues_page(
        &self,
        q: &issue::IssueQuery,
        limit: u32,
        offset: u64,
    ) -> crate::Result<Page<issue::Issue>> {
        self.fetch_page::<issue::IssuesEnvelope>("issues.json", &q.to_query(), limit, offset)
            .await
    }

    /// `GET /issues.json?issue_id=<comma-list>&status_id=*`, auto-paged (no
    /// `limit`/`offset` exposed to any caller of this method). Hydrates a
    /// list of issue ids — e.g. the thin results `search.json` returns — to
    /// full issue dicts. `status_id=*` overrides Redmine's default
    /// open-only filter, since the ids may reference closed issues. Returns
    /// an empty `Vec` without making an HTTP call when `ids` is empty.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn list_issues_by_id(&self, ids: &[IssueId]) -> crate::Result<Vec<issue::Issue>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut q = Query::default();
        q.insert(
            "issue_id",
            ids.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        );
        q.insert("status_id", "*");
        let page = self
            .fetch_all::<issue::IssuesEnvelope>("issues.json", &q)
            .await?;
        Ok(page.items)
    }

    /// `GET /issues.json?parent_id={id}&status_id=*`, auto-paged. Includes
    /// closed subtasks — Redmine's default issue filter is open-only, and
    /// the reference tool contract says subtasks include closed ones.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn list_subtasks(&self, parent: IssueId) -> crate::Result<Vec<issue::Issue>> {
        let mut q = Query::default();
        q.insert("parent_id", parent.to_string());
        q.insert("status_id", "*");
        let page = self
            .fetch_all::<issue::IssuesEnvelope>("issues.json", &q)
            .await?;
        Ok(page.items)
    }

    /// `GET /search.json`, a single explicit page — genuinely paginated on
    /// Redmine's side (unlike the four bare-collection endpoints), so this
    /// goes through `fetch_page`, not `get_collection`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn search_issues_page(
        &self,
        q: &search::SearchQuery,
        limit: u32,
        offset: u64,
    ) -> crate::Result<Page<search::SearchResult>> {
        self.fetch_page::<search::SearchResultsEnvelope>(
            "search.json",
            &q.to_query(),
            limit,
            offset,
        )
        .await
    }

    /// `GET /issues/{id}.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn get_issue(
        &self,
        id: IssueId,
        inc: &[issue::IssueInclude],
    ) -> crate::Result<issue::Issue> {
        let mut q = Query::default();
        if let Some(include) = issue::includes_to_query_value(inc) {
            q.insert("include", include);
        }
        let env: issue::IssueEnvelope = self.get_json(&format!("issues/{id}.json"), &q).await?;
        Ok(env.issue)
    }

    /// `POST /issues.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine rejects the payload
    /// (e.g. 422 with validation errors).
    pub async fn create_issue(&self, new: &issue::IssueCreate) -> crate::Result<issue::Issue> {
        let env: issue::IssueEnvelope = self
            .post_json("issues.json", &issue::IssueCreateEnvelope { issue: new })
            .await?;
        Ok(env.issue)
    }

    /// `PUT /issues/{id}.json`, then a follow-up `GET` to return the full
    /// updated resource — Redmine's `PUT` itself answers `204 No Content`
    /// (matching `update_version`/`update_membership`/`update_time_entry`).
    ///
    /// # Errors
    ///
    /// Returns an error if either request fails, or if Redmine rejects the
    /// update (e.g. 422 with validation errors).
    pub async fn update_issue(
        &self,
        id: IssueId,
        patch: &issue::IssueUpdate,
    ) -> crate::Result<issue::Issue> {
        self.put_json(
            &format!("issues/{id}.json"),
            &issue::IssueUpdateEnvelope { issue: patch },
        )
        .await?;
        self.get_issue(id, &[]).await
    }

    /// `DELETE /issues/{id}.json`. Redmine cascade-deletes descendant issues
    /// automatically (`Redmine::NestedSet::IssueNestedSet#destroy_children`,
    /// a `before_destroy` callback) — there is no separate "cascade" request
    /// parameter to send. The caller (`delete_redmine_issue`) is responsible
    /// for the confirmation dance and the impact preview; this method only
    /// ever sends the one `DELETE`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn delete_issue(&self, id: IssueId) -> crate::Result<()> {
        self.delete(&format!("issues/{id}.json")).await
    }

    /// `GET /issues/{issue_id}/relations.json` — no pagination envelope at
    /// all (not even `total_count`; verified against
    /// `issue_relations/index.api.rsb`).
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, Redmine responds with a
    /// non-success status, or the response unexpectedly carries a
    /// pagination envelope.
    pub async fn list_relations(
        &self,
        issue_id: IssueId,
    ) -> crate::Result<Vec<relation::IssueRelation>> {
        self.get_collection::<relation::IssueRelationsEnvelope>(
            &format!("issues/{issue_id}/relations.json"),
            &Query::default(),
        )
        .await
    }

    /// `POST /issues/{issue_id}/relations.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine rejects the payload
    /// (e.g. 422 for a same-project violation or a circular dependency).
    pub async fn create_relation(
        &self,
        issue_id: IssueId,
        new: &relation::IssueRelationCreate,
    ) -> crate::Result<relation::IssueRelation> {
        let env: relation::IssueRelationEnvelope = self
            .post_json(
                &format!("issues/{issue_id}/relations.json"),
                &relation::IssueRelationCreateEnvelope { relation: new },
            )
            .await?;
        Ok(env.relation)
    }

    /// `DELETE /relations/{id}.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status (403 if the relation is not deletable by this
    /// credential).
    pub async fn delete_relation(&self, id: RelationId) -> crate::Result<()> {
        self.delete(&format!("relations/{id}.json")).await
    }

    /// `POST /issues/{issue_id}/watchers.json`, body `{"user_id": ...}`.
    /// Redmine answers `204 No Content` on success, per
    /// `watchers_controller.rb#create`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status (403 without `add_issue_watchers`).
    pub async fn add_watcher(&self, issue_id: IssueId, user_id: UserId) -> crate::Result<()> {
        #[derive(Serialize)]
        struct Body {
            user_id: UserId,
        }
        self.post_json_no_content(
            &format!("issues/{issue_id}/watchers.json"),
            &Body { user_id },
        )
        .await
    }

    /// `DELETE /issues/{issue_id}/watchers/{user_id}.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status (403 without `delete_issue_watchers`, 404 for an
    /// unknown user id).
    pub async fn remove_watcher(&self, issue_id: IssueId, user_id: UserId) -> crate::Result<()> {
        self.delete(&format!("issues/{issue_id}/watchers/{user_id}.json"))
            .await
    }

    /// `PUT /journals/{id}.json`. Redmine answers `204 No Content` with no
    /// body (`journals_controller.rb#update`) — there is no `GET
    /// /journals/{id}.json` to follow up with, unlike every other `update_*`
    /// method in this client; the caller echoes back what it sent.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status (403 if the note is not editable by this
    /// credential; Redmine silently no-ops rather than 422 on a journal-only
    /// validation failure, per `journals_controller.rb`).
    pub async fn update_journal(
        &self,
        id: JournalId,
        patch: &journal::JournalUpdate,
    ) -> crate::Result<()> {
        self.put_json(
            &format!("journals/{id}.json"),
            &journal::JournalUpdateEnvelope { journal: patch },
        )
        .await
    }

    /// `GET /projects/{id}/issue_categories.json`. Carries a `total_count`
    /// but no `offset`/`limit` (the controller loads every category
    /// unconditionally, `@categories = @project.issue_categories.to_a`) —
    /// neither [`Self::fetch_all`] nor [`Self::get_collection`] fit, so this
    /// reads the envelope directly and ignores `total_count`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn list_issue_categories(
        &self,
        project: &ProjectIdent,
    ) -> crate::Result<Vec<issue_category::IssueCategory>> {
        let env: issue_category::IssueCategoriesEnvelope = self
            .get_json(
                &format!("projects/{project}/issue_categories.json"),
                &Query::default(),
            )
            .await?;
        Ok(env.into_items())
    }

    /// `GET /issue_categories/{id}.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn get_issue_category(
        &self,
        id: IssueCategoryId,
    ) -> crate::Result<issue_category::IssueCategory> {
        let env: issue_category::IssueCategoryEnvelope = self
            .get_json(&format!("issue_categories/{id}.json"), &Query::default())
            .await?;
        Ok(env.issue_category)
    }

    /// `POST /projects/{id}/issue_categories.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine rejects the payload
    /// (e.g. 422 with a blank name).
    pub async fn create_issue_category(
        &self,
        project: &ProjectIdent,
        new: &issue_category::IssueCategoryCreate,
    ) -> crate::Result<issue_category::IssueCategory> {
        let env: issue_category::IssueCategoryEnvelope = self
            .post_json(
                &format!("projects/{project}/issue_categories.json"),
                &issue_category::IssueCategoryCreateEnvelope {
                    issue_category: new,
                },
            )
            .await?;
        Ok(env.issue_category)
    }

    /// `PUT /issue_categories/{id}.json`, then a follow-up `GET` — same
    /// 204-then-fetch pattern as [`Self::update_version`].
    ///
    /// # Errors
    ///
    /// Returns an error if either request fails, or if Redmine rejects the
    /// update.
    pub async fn update_issue_category(
        &self,
        id: IssueCategoryId,
        patch: &issue_category::IssueCategoryUpdate,
    ) -> crate::Result<issue_category::IssueCategory> {
        self.put_json(
            &format!("issue_categories/{id}.json"),
            &issue_category::IssueCategoryUpdateEnvelope {
                issue_category: patch,
            },
        )
        .await?;
        self.get_issue_category(id).await
    }

    /// `DELETE /issue_categories/{id}.json`, optionally with a
    /// `reassign_to_id` query parameter to bulk-reassign the category's
    /// issues instead of leaving them uncategorised (a top-level query
    /// parameter, not nested under `issue_category` — confirmed against
    /// `test/integration/api_test/issue_categories_test.rb`).
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn delete_issue_category(
        &self,
        id: IssueCategoryId,
        reassign_to_id: Option<IssueCategoryId>,
    ) -> crate::Result<()> {
        let mut q = Query::default();
        if let Some(reassign_to_id) = reassign_to_id {
            q.insert("reassign_to_id", reassign_to_id.to_string());
        }
        self.delete_with_query(&format!("issue_categories/{id}.json"), &q)
            .await
    }

    /// `GET /time_entries.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn list_time_entries(
        &self,
        q: &time_entry::TimeEntryQuery,
    ) -> crate::Result<Page<time_entry::TimeEntry>> {
        self.fetch_all::<time_entry::TimeEntriesEnvelope>("time_entries.json", &q.to_query())
            .await
    }

    /// `GET /time_entries.json`, a single explicit page — unlike
    /// [`Self::list_time_entries`], never auto-pages. Used by
    /// `list_time_entries`, which exposes `limit`/`offset` directly.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn list_time_entries_page(
        &self,
        q: &time_entry::TimeEntryQuery,
        limit: u32,
        offset: u64,
    ) -> crate::Result<Page<time_entry::TimeEntry>> {
        self.fetch_page::<time_entry::TimeEntriesEnvelope>(
            "time_entries.json",
            &q.to_query(),
            limit,
            offset,
        )
        .await
    }

    /// `POST /time_entries.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine rejects the payload
    /// (e.g. 422 with validation errors).
    pub async fn create_time_entry(
        &self,
        new: &time_entry::TimeEntryCreate,
    ) -> crate::Result<time_entry::TimeEntry> {
        let env: time_entry::TimeEntryEnvelope = self
            .post_json(
                "time_entries.json",
                &time_entry::TimeEntryCreateEnvelope { time_entry: new },
            )
            .await?;
        Ok(env.time_entry)
    }

    /// `GET /time_entries/{id}.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn get_time_entry(&self, id: TimeEntryId) -> crate::Result<time_entry::TimeEntry> {
        let env: time_entry::TimeEntryEnvelope = self
            .get_json(&format!("time_entries/{id}.json"), &Query::default())
            .await?;
        Ok(env.time_entry)
    }

    /// `PUT /time_entries/{id}.json`, then a follow-up `GET` to return the
    /// full updated resource — Redmine's `PUT` itself answers `204 No
    /// Content` (matching `update_version`/`update_membership`).
    ///
    /// # Errors
    ///
    /// Returns an error if either request fails, or if Redmine rejects the
    /// update (e.g. 422 with validation errors).
    pub async fn update_time_entry(
        &self,
        id: TimeEntryId,
        patch: &time_entry::TimeEntryUpdate,
    ) -> crate::Result<time_entry::TimeEntry> {
        self.put_json(
            &format!("time_entries/{id}.json"),
            &time_entry::TimeEntryUpdateEnvelope { time_entry: patch },
        )
        .await?;
        self.get_time_entry(id).await
    }

    /// `GET /enumerations/time_entry_activities.json` — no pagination
    /// envelope. The project-scoped variant (`GET
    /// /projects/{id}.json?include=time_entry_activities`) reuses
    /// [`Self::get_project`] with [`project::ProjectInclude::TimeEntryActivities`]
    /// instead of a dedicated method — it is a plain `include=`, not a
    /// distinct endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, Redmine responds with a
    /// non-success status, or the response unexpectedly carries a
    /// pagination envelope.
    pub async fn list_time_entry_activities(&self) -> crate::Result<Vec<enumeration::Enumeration>> {
        self.get_collection::<enumeration::TimeEntryActivitiesEnvelope>(
            "enumerations/time_entry_activities.json",
            &Query::default(),
        )
        .await
    }

    /// `GET /trackers.json` — no pagination envelope.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, Redmine responds with a
    /// non-success status, or the response unexpectedly carries a
    /// pagination envelope.
    pub async fn list_trackers(&self) -> crate::Result<Vec<tracker::Tracker>> {
        self.get_collection::<tracker::TrackersEnvelope>("trackers.json", &Query::default())
            .await
    }

    /// `GET /issue_statuses.json` — no pagination envelope.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, Redmine responds with a
    /// non-success status, or the response unexpectedly carries a
    /// pagination envelope.
    pub async fn list_issue_statuses(&self) -> crate::Result<Vec<issue_status::IssueStatus>> {
        self.get_collection::<issue_status::IssueStatusesEnvelope>(
            "issue_statuses.json",
            &Query::default(),
        )
        .await
    }

    /// `GET /enumerations/issue_priorities.json` — no pagination envelope.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, Redmine responds with a
    /// non-success status, or the response unexpectedly carries a
    /// pagination envelope.
    pub async fn list_issue_priorities(&self) -> crate::Result<Vec<enumeration::Enumeration>> {
        self.get_collection::<enumeration::IssuePrioritiesEnvelope>(
            "enumerations/issue_priorities.json",
            &Query::default(),
        )
        .await
    }

    /// `GET /users.json`, a single explicit page (admin-only on Redmine's
    /// side).
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status (403 for a non-admin credential).
    pub async fn list_users(
        &self,
        q: &user::UserQuery,
        limit: u32,
        offset: u64,
    ) -> crate::Result<Page<user::User>> {
        self.fetch_page::<user::UsersEnvelope>("users.json", &q.to_query(), limit, offset)
            .await
    }

    /// `GET /queries.json`, auto-paged. Redmine's REST API has no
    /// create/update/delete for saved queries — this is read-only by nature.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn list_saved_queries(&self) -> crate::Result<Page<query::SavedQuery>> {
        self.fetch_all::<query::SavedQueriesEnvelope>("queries.json", &Query::default())
            .await
    }

    /// `GET /projects/{id}/versions.json`. Always returns every version —
    /// Redmine's endpoint has no `limit`/`offset` and no server-side status
    /// filter; a caller wanting a status filter applies it client-side.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn list_versions(
        &self,
        project: &ProjectIdent,
    ) -> crate::Result<Vec<version::Version>> {
        let env: version::VersionsEnvelope = self
            .get_json(
                &format!("projects/{project}/versions.json"),
                &Query::default(),
            )
            .await?;
        Ok(env.versions)
    }

    /// `GET /versions/{id}.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn get_version(&self, id: VersionId) -> crate::Result<version::Version> {
        let env: version::VersionEnvelope = self
            .get_json(&format!("versions/{id}.json"), &Query::default())
            .await?;
        Ok(env.version)
    }

    /// `POST /projects/{id}/versions.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine rejects the payload
    /// (e.g. 422 with validation errors).
    pub async fn create_version(
        &self,
        project: &ProjectIdent,
        new: &version::VersionWrite,
    ) -> crate::Result<version::Version> {
        let env: version::VersionEnvelope = self
            .post_json(
                &format!("projects/{project}/versions.json"),
                &version::VersionWriteEnvelope { version: new },
            )
            .await?;
        Ok(env.version)
    }

    /// `PUT /versions/{id}.json`, then a follow-up `GET` to return the full
    /// updated resource — Redmine's `PUT` itself answers `204 No Content`.
    ///
    /// # Errors
    ///
    /// Returns an error if either request fails, or if Redmine rejects the
    /// update (e.g. 422 with validation errors).
    pub async fn update_version(
        &self,
        id: VersionId,
        patch: &version::VersionWrite,
    ) -> crate::Result<version::Version> {
        self.put_json(
            &format!("versions/{id}.json"),
            &version::VersionWriteEnvelope { version: patch },
        )
        .await?;
        self.get_version(id).await
    }

    /// `DELETE /versions/{id}.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn delete_version(&self, id: VersionId) -> crate::Result<()> {
        self.delete(&format!("versions/{id}.json")).await
    }

    /// `GET /projects/{id}/memberships.json`, auto-paged. No tool exposes
    /// `limit`/`offset` for this endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn list_memberships(
        &self,
        project: &ProjectIdent,
    ) -> crate::Result<Page<membership::Membership>> {
        self.fetch_all::<membership::MembershipsEnvelope>(
            &format!("projects/{project}/memberships.json"),
            &Query::default(),
        )
        .await
    }

    /// `GET /memberships/{id}.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn get_membership(&self, id: MembershipId) -> crate::Result<membership::Membership> {
        let env: membership::MembershipEnvelope = self
            .get_json(&format!("memberships/{id}.json"), &Query::default())
            .await?;
        Ok(env.membership)
    }

    /// `POST /projects/{id}/memberships.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine rejects the payload
    /// (e.g. 422 with validation errors).
    pub async fn create_membership(
        &self,
        project: &ProjectIdent,
        new: &membership::MembershipCreate,
    ) -> crate::Result<membership::Membership> {
        let env: membership::MembershipEnvelope = self
            .post_json(
                &format!("projects/{project}/memberships.json"),
                &membership::MembershipCreateEnvelope { membership: new },
            )
            .await?;
        Ok(env.membership)
    }

    /// `PUT /memberships/{id}.json`, then a follow-up `GET` — same 204
    /// pattern as [`Self::update_version`].
    ///
    /// # Errors
    ///
    /// Returns an error if either request fails, or if Redmine rejects the
    /// update (e.g. 422 with validation errors).
    pub async fn update_membership(
        &self,
        id: MembershipId,
        patch: &membership::MembershipUpdate,
    ) -> crate::Result<membership::Membership> {
        self.put_json(
            &format!("memberships/{id}.json"),
            &membership::MembershipUpdateEnvelope { membership: patch },
        )
        .await?;
        self.get_membership(id).await
    }

    /// `DELETE /memberships/{id}.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn delete_membership(&self, id: MembershipId) -> crate::Result<()> {
        self.delete(&format!("memberships/{id}.json")).await
    }

    /// `GET /roles.json` — no pagination envelope. Unlike `list_redmine_users`,
    /// this is **not** admin-gated: `RolesController` lets any authenticated
    /// API request through (`require_admin_or_api_request`).
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, Redmine responds with a
    /// non-success status, or the response unexpectedly carries a
    /// pagination envelope.
    pub async fn list_roles(&self) -> crate::Result<Vec<role::Role>> {
        self.get_collection::<role::RolesEnvelope>("roles.json", &Query::default())
            .await
    }

    /// `GET /custom_fields.json` — no pagination envelope, but admin-only on
    /// Redmine's side (403 for a non-admin credential). Returns *every*
    /// custom field definition on the instance, regardless of
    /// `customized_type` or project scope; callers filter client-side.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, Redmine responds with a
    /// non-success status (403 for a non-admin credential), or the response
    /// unexpectedly carries a pagination envelope.
    pub async fn list_custom_field_definitions(
        &self,
    ) -> crate::Result<Vec<custom_field::CustomFieldDefinition>> {
        self.get_collection::<custom_field::CustomFieldDefinitionsEnvelope>(
            "custom_fields.json",
            &Query::default(),
        )
        .await
    }

    /// `GET /search.json`, a single explicit page, restricted to the
    /// resource(s) named in `q.resources` (`search_entire_redmine`). See
    /// [`Self::search_issues_page`] for the issues-only counterpart.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn search_entire_page(
        &self,
        q: &search::EntireSearchQuery,
        limit: u32,
        offset: u64,
    ) -> crate::Result<Page<search::SearchResult>> {
        self.fetch_page::<search::SearchResultsEnvelope>(
            "search.json",
            &q.to_query(),
            limit,
            offset,
        )
        .await
    }

    /// `GET /projects/{id}/wiki/index.json` — no pagination envelope
    /// (`wiki/index.api.rsb` is a bare `api.array`).
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, Redmine responds with a
    /// non-success status, or the response unexpectedly carries a
    /// pagination envelope.
    pub async fn list_wiki_pages(
        &self,
        project: &ProjectIdent,
    ) -> crate::Result<Vec<wiki::WikiPageListItem>> {
        self.get_collection::<wiki::WikiPagesEnvelope>(
            &format!("projects/{project}/wiki/index.json"),
            &Query::default(),
        )
        .await
    }

    /// `GET /projects/{id}/wiki/{title}.json`, or
    /// `GET /projects/{id}/wiki/{title}/{version}.json` when `version` is
    /// given — a path segment, not a query parameter.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn get_wiki_page(
        &self,
        project: &ProjectIdent,
        title: &WikiTitle,
        version: Option<u32>,
        include_attachments: bool,
    ) -> crate::Result<wiki::WikiPage> {
        let mut q = Query::default();
        if include_attachments {
            q.insert("include", "attachments");
        }
        let segment = title.encoded_segment();
        let path = version.map_or_else(
            || format!("projects/{project}/wiki/{segment}.json"),
            |v| format!("projects/{project}/wiki/{segment}/{v}.json"),
        );
        let env: wiki::WikiPageEnvelope = self.get_json(&path, &q).await?;
        Ok(env.wiki_page)
    }

    /// `PUT /projects/{id}/wiki/{title}.json`, with no follow-up `GET`. The
    /// `rename` mechanism uses this directly rather than
    /// [`Self::upsert_wiki_page`]: `rename` re-fetches at the *new* title
    /// afterward regardless, so a follow-up `GET` at `title` (still the
    /// *old* title at this point) would be pure waste.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, or if Redmine rejects the
    /// write (e.g. 422 with validation errors).
    pub async fn write_wiki_page(
        &self,
        project: &ProjectIdent,
        title: &WikiTitle,
        write: &wiki::WikiPageWrite,
    ) -> crate::Result<()> {
        self.put_json(
            &format!("projects/{project}/wiki/{}.json", title.encoded_segment()),
            &wiki::WikiPageWriteEnvelope { wiki_page: write },
        )
        .await
    }

    /// [`Self::write_wiki_page`], then a follow-up `GET` for the full page.
    /// Redmine's `PUT` answers `204`-equivalent `render_api_ok` (no body)
    /// when updating an existing page, and a full body only when creating
    /// one for the first time; fetching afterward unconditionally keeps one
    /// code path for both `create` and `update`.
    ///
    /// # Errors
    ///
    /// Returns an error if either request fails, or if Redmine rejects the
    /// write (e.g. 422 with validation errors).
    pub async fn upsert_wiki_page(
        &self,
        project: &ProjectIdent,
        title: &WikiTitle,
        write: &wiki::WikiPageWrite,
    ) -> crate::Result<wiki::WikiPage> {
        self.write_wiki_page(project, title, write).await?;
        self.get_wiki_page(project, title, None, false).await
    }

    /// `DELETE /projects/{id}/wiki/{title}.json`. Children are un-parented
    /// (`parent_id` set to `NULL`), never cascade-deleted.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status.
    pub async fn delete_wiki_page(
        &self,
        project: &ProjectIdent,
        title: &WikiTitle,
    ) -> crate::Result<()> {
        self.delete(&format!(
            "projects/{project}/wiki/{}.json",
            title.encoded_segment()
        ))
        .await
    }

    /// `GET /attachments/{id}.json`. See [`attachment::Attachment`]'s doc
    /// comment for which fields this endpoint never populates.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status (404 for an unknown or already-deleted id).
    pub async fn get_attachment(&self, id: AttachmentId) -> crate::Result<attachment::Attachment> {
        let env: attachment::AttachmentEnvelope = self
            .get_json(&format!("attachments/{id}.json"), &Query::default())
            .await?;
        Ok(env.attachment)
    }

    /// `DELETE /attachments/{id}.json`. Redmine's endpoint deletes *any*
    /// attachment regardless of its container — callers that need to
    /// restrict this to project Files should check
    /// [`attachment::Attachment`] before calling (see that type's doc
    /// comment on why a container-type check can't be done from Redmine's
    /// response alone).
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine responds with a
    /// non-success status (403 if the attachment is not deletable by this
    /// credential).
    pub async fn delete_attachment(&self, id: AttachmentId) -> crate::Result<()> {
        self.delete(&format!("attachments/{id}.json")).await
    }

    /// `GET /projects/{id}/files.json` — no pagination envelope
    /// (`files/index.api.rsb` is a bare `api.array :files`).
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, Redmine responds with a
    /// non-success status, or the response unexpectedly carries a
    /// pagination envelope.
    pub async fn list_project_files(
        &self,
        project: &ProjectIdent,
    ) -> crate::Result<Vec<attachment::Attachment>> {
        self.get_collection::<attachment::ProjectFilesEnvelope>(
            &format!("projects/{project}/files.json"),
            &Query::default(),
        )
        .await
    }

    /// `POST /uploads.json`, step one of attaching a file (see
    /// `docs/tool-contract.md` for the request-body-size ceiling this sits
    /// behind on the HTTP transport). Redmine
    /// 406s any request whose `Content-Type` is not exactly
    /// `application/octet-stream` — this method sets it unconditionally, so
    /// `content_type` only ever affects the *stored* attachment's recorded
    /// MIME type, never the request Redmine receives.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, or if Redmine rejects the
    /// upload (422 when `body` exceeds `Setting.attachment_max_size`).
    pub async fn create_upload(
        &self,
        body: Bytes,
        filename: Option<&str>,
        content_type: Option<&str>,
    ) -> crate::Result<upload::Upload> {
        let mut q = Query::default();
        if let Some(filename) = filename {
            q.insert("filename", filename);
        }
        if let Some(content_type) = content_type {
            q.insert("content_type", content_type);
        }
        let env: upload::UploadEnvelope = self
            .post_bytes("uploads.json", &q, "application/octet-stream", body)
            .await?;
        Ok(env.upload)
    }

    /// `POST /projects/{id}/files.json`, step two of attaching a file:
    /// consumes the token from [`Self::create_upload`] to attach that
    /// already-uploaded file to the project (or one of its versions, via
    /// `new.version_id`). Answers `204 No Content`
    /// (`FilesController#create` → `render_api_ok`) — there is no follow-up
    /// `GET` here because the attachment id was already known from the
    /// upload step; callers that want the full resource call
    /// [`Self::get_attachment`] with that id.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, or if Redmine rejects the
    /// attach (400 for an unknown/expired token, 422 with validation
    /// errors).
    pub async fn create_project_file(
        &self,
        project: &ProjectIdent,
        new: &upload::ProjectFileCreate,
    ) -> crate::Result<()> {
        self.post_json_no_content(
            &format!("projects/{project}/files.json"),
            &upload::ProjectFileCreateEnvelope { file: new },
        )
        .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::str::FromStr as _;

    use secrecy::SecretString;

    use super::*;

    fn scoped_client(base: &str) -> RedmineClient {
        RedmineClientBuilder::new(base.parse().unwrap())
            .credential(Credential::ApiKey(SecretString::from("k")))
            .build()
            .expect("client should build")
    }

    #[test]
    fn endpoint_rejects_absolute_paths() {
        let client = scoped_client("https://example.com/redmine/");
        let cred = Credential::ApiKey(SecretString::from("k"));
        let scoped = client.as_user(&cred);
        assert!(scoped.endpoint("/etc/passwd").is_err());
    }

    #[test]
    fn endpoint_rejects_dotdot_traversal() {
        let client = scoped_client("https://example.com/redmine/");
        let cred = Credential::ApiKey(SecretString::from("k"));
        let scoped = client.as_user(&cred);
        assert!(scoped.endpoint("../admin").is_err());
        assert!(scoped.endpoint("issues/../../admin").is_err());
    }

    #[test]
    fn endpoint_rejects_double_slash_and_control_chars() {
        let client = scoped_client("https://example.com/redmine/");
        let cred = Credential::ApiKey(SecretString::from("k"));
        let scoped = client.as_user(&cred);
        assert!(scoped.endpoint("issues//1.json").is_err());
        assert!(scoped.endpoint("issues/1\n.json").is_err());
    }

    #[test]
    fn endpoint_preserves_base_sub_path() {
        let client = scoped_client("https://example.com/redmine");
        let cred = Credential::ApiKey(SecretString::from("k"));
        let scoped = client.as_user(&cred);
        let url = scoped
            .endpoint("issues.json")
            .expect("relative path should be accepted");
        assert_eq!(url.as_str(), "https://example.com/redmine/issues.json");
    }

    #[test]
    fn endpoint_with_no_sub_path_still_works() {
        let client = scoped_client("https://example.com");
        let cred = Credential::ApiKey(SecretString::from("k"));
        let scoped = client.as_user(&cred);
        let url = scoped
            .endpoint("issues.json")
            .expect("relative path should be accepted");
        assert_eq!(url.as_str(), "https://example.com/issues.json");
    }

    // --- get_collection / fetch_page ---

    #[derive(Debug, serde::Deserialize)]
    struct TestWidget {
        #[allow(dead_code, reason = "only the item count matters to these tests")]
        id: u64,
    }

    #[derive(Debug, serde::Deserialize)]
    struct TestBareEnvelope {
        widgets: Vec<TestWidget>,
    }

    impl BareCollection for TestBareEnvelope {
        type Item = TestWidget;

        fn into_items(self) -> Vec<TestWidget> {
            self.widgets
        }
    }

    #[derive(Debug, serde::Deserialize)]
    struct TestPagedEnvelope {
        widgets: Vec<TestWidget>,
        total_count: u64,
        offset: u64,
        limit: u32,
    }

    impl Collection for TestPagedEnvelope {
        type Item = TestWidget;

        fn total_count(&self) -> u64 {
            self.total_count
        }

        fn offset(&self) -> u64 {
            self.offset
        }

        fn limit(&self) -> u32 {
            self.limit
        }

        fn into_items(self) -> Vec<TestWidget> {
            self.widgets
        }
    }

    #[tokio::test]
    async fn get_collection_sends_no_pagination_params_and_returns_items() {
        let server = wiremock::MockServer::start().await;
        let base = server.uri().parse().unwrap();
        let client = RedmineClientBuilder::new(base)
            .credential(Credential::ApiKey(SecretString::from("k")))
            .build()
            .unwrap();

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/widgets.json"))
            .and(wiremock::matchers::query_param_is_missing("limit"))
            .and(wiremock::matchers::query_param_is_missing("offset"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "widgets": [{"id": 1}, {"id": 2}]
                })),
            )
            .mount(&server)
            .await;

        let cred = Credential::ApiKey(SecretString::from("k"));
        let items = client
            .as_user(&cred)
            .get_collection::<TestBareEnvelope>("widgets.json", &Query::default())
            .await
            .expect("un-paginated collection should be fetched");
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn get_collection_errors_loudly_on_a_paginated_envelope() {
        let server = wiremock::MockServer::start().await;
        let base = server.uri().parse().unwrap();
        let client = RedmineClientBuilder::new(base)
            .credential(Credential::ApiKey(SecretString::from("k")))
            .build()
            .unwrap();

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/widgets.json"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "widgets": [{"id": 1}],
                    "total_count": 50,
                    "offset": 0,
                    "limit": 1
                })),
            )
            .mount(&server)
            .await;

        let cred = Credential::ApiKey(SecretString::from("k"));
        let err = client
            .as_user(&cred)
            .get_collection::<TestBareEnvelope>("widgets.json", &Query::default())
            .await
            .expect_err("a paginated envelope must not be silently treated as complete");
        assert!(matches!(err, Error::Decode { .. }));
    }

    #[tokio::test]
    async fn fetch_page_sends_exactly_the_requested_limit_and_offset_and_does_not_follow_on() {
        let server = wiremock::MockServer::start().await;
        let base = server.uri().parse().unwrap();
        let client = RedmineClientBuilder::new(base)
            .credential(Credential::ApiKey(SecretString::from("k")))
            .build()
            .unwrap();

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/widgets.json"))
            .and(wiremock::matchers::query_param("limit", "10"))
            .and(wiremock::matchers::query_param("offset", "20"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "widgets": [{"id": 21}],
                    "total_count": 100,
                    "offset": 20,
                    "limit": 10
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let cred = Credential::ApiKey(SecretString::from("k"));
        let page = client
            .as_user(&cred)
            .fetch_page::<TestPagedEnvelope>("widgets.json", &Query::default(), 10, 20)
            .await
            .expect("single page fetch should succeed");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.total_count, 100);
        assert_eq!(page.offset, 20);
        assert_eq!(page.limit, 10);
        assert!(!page.truncated);
    }

    // --- Discovery-tool API methods ---

    fn discovery_client(server: &wiremock::MockServer) -> RedmineClient {
        RedmineClientBuilder::new(server.uri().parse().unwrap())
            .credential(Credential::ApiKey(SecretString::from("k")))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn list_trackers_sends_no_pagination_params() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/trackers.json"))
            .and(wiremock::matchers::query_param_is_missing("limit"))
            .and(wiremock::matchers::query_param_is_missing("offset"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "trackers": [{"id": 1, "name": "Bug"}]
                })),
            )
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let trackers = client
            .as_user(&cred)
            .list_trackers()
            .await
            .expect("list_trackers should succeed");
        assert_eq!(trackers.len(), 1);
    }

    #[tokio::test]
    async fn list_issue_statuses_sends_no_pagination_params() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/issue_statuses.json"))
            .and(wiremock::matchers::query_param_is_missing("limit"))
            .and(wiremock::matchers::query_param_is_missing("offset"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "issue_statuses": [{"id": 1, "name": "New", "is_closed": false}]
                })),
            )
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let statuses = client
            .as_user(&cred)
            .list_issue_statuses()
            .await
            .expect("list_issue_statuses should succeed");
        assert_eq!(statuses.len(), 1);
    }

    #[tokio::test]
    async fn list_issue_priorities_sends_no_pagination_params() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/enumerations/issue_priorities.json",
            ))
            .and(wiremock::matchers::query_param_is_missing("limit"))
            .and(wiremock::matchers::query_param_is_missing("offset"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "issue_priorities": [{"id": 1, "name": "Normal"}]
                })),
            )
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let priorities = client
            .as_user(&cred)
            .list_issue_priorities()
            .await
            .expect("list_issue_priorities should succeed");
        assert_eq!(priorities.len(), 1);
    }

    #[tokio::test]
    async fn list_users_sends_exactly_the_requested_limit_offset_and_filters() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/users.json"))
            .and(wiremock::matchers::query_param("limit", "10"))
            .and(wiremock::matchers::query_param("offset", "0"))
            .and(wiremock::matchers::query_param("name", "Ale& Ünïcode"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "users": [{
                        "id": 1, "login": "alice", "firstname": "Alice", "lastname": "Example",
                        "created_on": "2026-01-01T00:00:00Z"
                    }],
                    "total_count": 1,
                    "offset": 0,
                    "limit": 10
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let q = user::UserQuery {
            name: Some("Ale& Ünïcode".to_string()),
            group_id: None,
            status: None,
        };
        let page = client
            .as_user(&cred)
            .list_users(&q, 10, 0)
            .await
            .expect("list_users should succeed");
        assert_eq!(page.items.len(), 1);
        assert!(!page.truncated);
    }

    #[tokio::test]
    async fn list_saved_queries_auto_pages() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/queries.json"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "queries": [{"id": 1, "name": "My open issues"}],
                    "total_count": 1,
                    "offset": 0,
                    "limit": 100
                })),
            )
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let page = client
            .as_user(&cred)
            .list_saved_queries()
            .await
            .expect("list_saved_queries should succeed");
        assert_eq!(page.items.len(), 1);
        assert!(!page.truncated);
    }

    // --- Project-management tool API methods ---

    #[tokio::test]
    async fn list_versions_tolerates_a_total_count_field_with_no_offset_or_limit() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/projects/5/versions.json"))
            .and(wiremock::matchers::query_param_is_missing("limit"))
            .and(wiremock::matchers::query_param_is_missing("offset"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "versions": [{
                        "id": 1, "project": {"id": 5, "name": "P"}, "name": "1.0",
                        "status": "open",
                        "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
                    }],
                    "total_count": 1
                })),
            )
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let project = ProjectIdent::Id(crate::ids::ProjectId(5));
        let versions = client
            .as_user(&cred)
            .list_versions(&project)
            .await
            .expect("a total_count field must not be rejected (it is not a Collection)");
        assert_eq!(versions.len(), 1);
    }

    #[tokio::test]
    async fn create_version_sends_expected_body() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path(
                "/projects/my-project/versions.json",
            ))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "version": {"name": "v2.0", "status": "open"}
            })))
            .respond_with(
                wiremock::ResponseTemplate::new(201).set_body_json(serde_json::json!({
                    "version": {
                        "id": 42, "project": {"id": 5, "name": "P"}, "name": "v2.0",
                        "status": "open",
                        "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
                    }
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let project = ProjectIdent::Identifier(
            crate::ids::ProjectIdentifier::from_str("my-project").unwrap(),
        );
        let write = version::VersionWrite {
            name: Some("v2.0".to_string()),
            status: Some(version::VersionStatus::Open),
            ..Default::default()
        };
        let created = client
            .as_user(&cred)
            .create_version(&project, &write)
            .await
            .expect("create_version should succeed");
        assert_eq!(created.id, 42);
    }

    #[tokio::test]
    async fn update_version_issues_a_put_then_exactly_one_get() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("PUT"))
            .and(wiremock::matchers::path("/versions/42.json"))
            .respond_with(wiremock::ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/versions/42.json"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "version": {
                        "id": 42, "project": {"id": 5, "name": "P"}, "name": "v2.0",
                        "status": "locked",
                        "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
                    }
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let patch = version::VersionWrite {
            status: Some(version::VersionStatus::Locked),
            ..Default::default()
        };
        let updated = client
            .as_user(&cred)
            .update_version(VersionId(42), &patch)
            .await
            .expect("update_version should succeed");
        assert_eq!(updated.status, "locked");
    }

    #[tokio::test]
    async fn delete_version_succeeds_on_204() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("DELETE"))
            .and(wiremock::matchers::path("/versions/42.json"))
            .respond_with(wiremock::ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        client
            .as_user(&cred)
            .delete_version(VersionId(42))
            .await
            .expect("delete_version should succeed");
    }

    #[tokio::test]
    async fn list_memberships_sends_no_pagination_params_but_auto_pages() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/projects/5/memberships.json"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "memberships": [{
                        "id": 1, "project": {"id": 5, "name": "P"},
                        "user": {"id": 2, "name": "Alice"},
                        "roles": [{"id": 3, "name": "Manager"}]
                    }],
                    "total_count": 1, "offset": 0, "limit": 100
                })),
            )
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let project = ProjectIdent::Id(crate::ids::ProjectId(5));
        let page = client
            .as_user(&cred)
            .list_memberships(&project)
            .await
            .expect("list_memberships should succeed");
        assert_eq!(page.items.len(), 1);
    }

    #[tokio::test]
    async fn create_membership_routes_group_id_through_the_user_id_field() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/projects/5/memberships.json"))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "membership": {"user_id": 20, "role_ids": [3, 4]}
            })))
            .respond_with(
                wiremock::ResponseTemplate::new(201).set_body_json(serde_json::json!({
                    "membership": {
                        "id": 7, "project": {"id": 5, "name": "P"},
                        "group": {"id": 20, "name": "Dev Team"},
                        "roles": [{"id": 3, "name": "Manager"}, {"id": 4, "name": "Developer"}]
                    }
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let project = ProjectIdent::Id(crate::ids::ProjectId(5));
        let new = membership::MembershipCreate {
            user_id: 20,
            role_ids: vec![3, 4],
        };
        let created = client
            .as_user(&cred)
            .create_membership(&project, &new)
            .await
            .expect("create_membership should succeed");
        assert_eq!(created.id, 7);
    }

    #[tokio::test]
    async fn update_membership_issues_a_put_then_exactly_one_get() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("PUT"))
            .and(wiremock::matchers::path("/memberships/7.json"))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "membership": {"role_ids": [4]}
            })))
            .respond_with(wiremock::ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/memberships/7.json"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "membership": {
                        "id": 7, "project": {"id": 5, "name": "P"},
                        "user": {"id": 2, "name": "Alice"},
                        "roles": [{"id": 4, "name": "Developer"}]
                    }
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let patch = membership::MembershipUpdate { role_ids: vec![4] };
        let updated = client
            .as_user(&cred)
            .update_membership(MembershipId(7), &patch)
            .await
            .expect("update_membership should succeed");
        assert_eq!(updated.id, 7);
    }

    #[tokio::test]
    async fn delete_membership_succeeds_on_200() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("DELETE"))
            .and(wiremock::matchers::path("/memberships/7.json"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        client
            .as_user(&cred)
            .delete_membership(MembershipId(7))
            .await
            .expect("delete_membership should succeed (Redmine answers 200, not 204)");
    }

    #[tokio::test]
    async fn list_roles_sends_no_pagination_params() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/roles.json"))
            .and(wiremock::matchers::query_param_is_missing("limit"))
            .and(wiremock::matchers::query_param_is_missing("offset"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "roles": [{"id": 3, "name": "Manager"}]
                })),
            )
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let roles = client
            .as_user(&cred)
            .list_roles()
            .await
            .expect("list_roles should succeed");
        assert_eq!(roles.len(), 1);
    }

    #[tokio::test]
    async fn list_custom_field_definitions_sends_no_pagination_params() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/custom_fields.json"))
            .and(wiremock::matchers::query_param_is_missing("limit"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "custom_fields": [{
                        "id": 6, "name": "Size", "field_format": "list",
                        "customized_type": "issue", "is_for_all": true
                    }]
                })),
            )
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let fields = client
            .as_user(&cred)
            .list_custom_field_definitions()
            .await
            .expect("list_custom_field_definitions should succeed");
        assert_eq!(fields.len(), 1);
    }

    #[tokio::test]
    async fn list_custom_field_definitions_forbidden_for_a_non_admin() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/custom_fields.json"))
            .respond_with(wiremock::ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let err = client
            .as_user(&cred)
            .list_custom_field_definitions()
            .await
            .expect_err("a non-admin credential should be forbidden");
        assert!(matches!(err, Error::Forbidden));
    }

    #[tokio::test]
    async fn list_issues_page_sends_exactly_the_requested_limit_and_offset() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/issues.json"))
            .and(wiremock::matchers::query_param("limit", "1"))
            .and(wiremock::matchers::query_param("offset", "0"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "issues": [],
                    "total_count": 42, "offset": 0, "limit": 1
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let page = client
            .as_user(&cred)
            .list_issues_page(&issue::IssueQuery::default(), 1, 0)
            .await
            .expect("list_issues_page should succeed");
        assert_eq!(page.total_count, 42);
        assert!(page.items.is_empty());
    }

    #[tokio::test]
    async fn list_issues_by_id_sends_one_comma_joined_issue_id_param_and_status_id_star() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/issues.json"))
            .and(wiremock::matchers::query_param("issue_id", "1,2,3"))
            .and(wiremock::matchers::query_param("status_id", "*"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "issues": [], "total_count": 0, "offset": 0, "limit": 100
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let ids = [
            crate::ids::IssueId(1),
            crate::ids::IssueId(2),
            crate::ids::IssueId(3),
        ];
        client
            .as_user(&cred)
            .list_issues_by_id(&ids)
            .await
            .expect("list_issues_by_id should succeed");
    }

    #[tokio::test]
    async fn list_issues_by_id_makes_no_http_call_for_an_empty_id_list() {
        let server = wiremock::MockServer::start().await;
        // No mock mounted: any request would fail this test.
        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let issues = client
            .as_user(&cred)
            .list_issues_by_id(&[])
            .await
            .expect("empty id list should short-circuit");
        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn list_subtasks_sends_parent_id_and_status_id_star() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/issues.json"))
            .and(wiremock::matchers::query_param("parent_id", "42"))
            .and(wiremock::matchers::query_param("status_id", "*"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "issues": [], "total_count": 0, "offset": 0, "limit": 100
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        client
            .as_user(&cred)
            .list_subtasks(crate::ids::IssueId(42))
            .await
            .expect("list_subtasks should succeed");
    }

    #[tokio::test]
    async fn search_issues_page_sends_exactly_the_requested_limit_and_offset() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/search.json"))
            .and(wiremock::matchers::query_param("q", "bug"))
            .and(wiremock::matchers::query_param("issues", "1"))
            .and(wiremock::matchers::query_param("limit", "10"))
            .and(wiremock::matchers::query_param("offset", "0"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "results": [], "total_count": 0, "offset": 0, "limit": 10
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let q = search::SearchQuery {
            q: "bug".to_string(),
            scope: None,
            open_issues: false,
        };
        let page = client
            .as_user(&cred)
            .search_issues_page(&q, 10, 0)
            .await
            .expect("search_issues_page should succeed");
        assert!(page.items.is_empty());
    }

    // --- Time-tracking API methods ---

    fn sample_time_entry_json(id: u64, hours: f64) -> serde_json::Value {
        serde_json::json!({
            "time_entry": {
                "id": id, "project": {"id": 5, "name": "P"},
                "user": {"id": 2, "name": "Alice"}, "activity": {"id": 9, "name": "Development"},
                "hours": hours, "spent_on": "2026-01-15",
                "created_on": "2026-01-15T00:00:00Z", "updated_on": "2026-01-15T00:00:00Z"
            }
        })
    }

    #[tokio::test]
    async fn list_time_entries_page_sends_exactly_the_requested_limit_and_offset_and_does_not_follow_on()
     {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/time_entries.json"))
            .and(wiremock::matchers::query_param("limit", "10"))
            .and(wiremock::matchers::query_param("offset", "0"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "time_entries": [], "total_count": 50, "offset": 0, "limit": 10
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let page = client
            .as_user(&cred)
            .list_time_entries_page(&time_entry::TimeEntryQuery::default(), 10, 0)
            .await
            .expect("list_time_entries_page should succeed");
        assert_eq!(page.total_count, 50);
        assert!(!page.truncated);
    }

    #[tokio::test]
    async fn get_time_entry_happy_path() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/time_entries/7.json"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(sample_time_entry_json(7, 2.5)),
            )
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let entry = client
            .as_user(&cred)
            .get_time_entry(crate::ids::TimeEntryId(7))
            .await
            .expect("get_time_entry should succeed");
        assert_eq!(entry.id, 7);
    }

    #[tokio::test]
    async fn update_time_entry_issues_a_put_then_exactly_one_get() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("PUT"))
            .and(wiremock::matchers::path("/time_entries/7.json"))
            .respond_with(wiremock::ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/time_entries/7.json"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(sample_time_entry_json(7, 3.0)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let patch = time_entry::TimeEntryUpdate {
            hours: Some(3.0),
            ..Default::default()
        };
        let updated = client
            .as_user(&cred)
            .update_time_entry(crate::ids::TimeEntryId(7), &patch)
            .await
            .expect("update_time_entry should succeed");
        assert!((updated.hours - 3.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn list_time_entry_activities_sends_no_pagination_params() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/enumerations/time_entry_activities.json",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "time_entry_activities": [{"id": 9, "name": "Development", "is_default": true, "active": true}]
                })),
            )
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let activities = client
            .as_user(&cred)
            .list_time_entry_activities()
            .await
            .expect("list_time_entry_activities should succeed");
        assert_eq!(activities.len(), 1);
        assert_eq!(activities.first().unwrap().name, "Development");
    }

    #[tokio::test]
    async fn list_time_entry_activities_errors_loudly_on_a_paginated_envelope() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/enumerations/time_entry_activities.json",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "time_entry_activities": [], "total_count": 0
                })),
            )
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let result = client.as_user(&cred).list_time_entry_activities().await;
        assert!(result.is_err());
    }

    // --- Search & wiki API methods ---

    #[tokio::test]
    async fn search_entire_page_sends_a_flag_for_each_requested_resource() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/search.json"))
            .and(wiremock::matchers::query_param("q", "install"))
            .and(wiremock::matchers::query_param("issues", "1"))
            .and(wiremock::matchers::query_param("wiki_pages", "1"))
            .and(wiremock::matchers::query_param("limit", "10"))
            .and(wiremock::matchers::query_param("offset", "0"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "results": [], "total_count": 0, "offset": 0, "limit": 10
                })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let q = search::EntireSearchQuery {
            q: "install".to_string(),
            resources: vec![
                search::SearchResource::Issues,
                search::SearchResource::WikiPages,
            ],
        };
        let page = client
            .as_user(&cred)
            .search_entire_page(&q, 10, 0)
            .await
            .expect("search_entire_page should succeed");
        assert!(page.items.is_empty());
    }

    fn sample_wiki_page_json() -> serde_json::Value {
        serde_json::json!({
            "wiki_page": {
                "title": "Home", "text": "Welcome", "version": 1,
                "created_on": "2026-01-01T00:00:00Z"
            }
        })
    }

    #[tokio::test]
    async fn list_wiki_pages_sends_no_pagination_params() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/projects/my-project/wiki/index.json",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "wiki_pages": [
                        {"title": "Home", "version": 1, "created_on": "2026-01-01T00:00:00Z"}
                    ]
                })),
            )
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let project = ProjectIdent::Identifier(
            crate::ids::ProjectIdentifier::from_str("my-project").unwrap(),
        );
        let pages = client
            .as_user(&cred)
            .list_wiki_pages(&project)
            .await
            .expect("list_wiki_pages should succeed");
        assert_eq!(pages.len(), 1);
    }

    #[tokio::test]
    async fn get_wiki_page_without_version_omits_the_version_segment() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/projects/my-project/wiki/Home.json",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(sample_wiki_page_json()),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let project = ProjectIdent::Identifier(
            crate::ids::ProjectIdentifier::from_str("my-project").unwrap(),
        );
        let title = WikiTitle::new("Home").unwrap();
        let page = client
            .as_user(&cred)
            .get_wiki_page(&project, &title, None, false)
            .await
            .expect("get_wiki_page should succeed");
        assert_eq!(page.title, "Home");
    }

    #[tokio::test]
    async fn get_wiki_page_with_version_requests_the_version_path_segment() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/projects/my-project/wiki/Home/3.json",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(sample_wiki_page_json()),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let project = ProjectIdent::Identifier(
            crate::ids::ProjectIdentifier::from_str("my-project").unwrap(),
        );
        let title = WikiTitle::new("Home").unwrap();
        client
            .as_user(&cred)
            .get_wiki_page(&project, &title, Some(3), false)
            .await
            .expect("get_wiki_page should succeed");
    }

    #[tokio::test]
    async fn get_wiki_page_encodes_a_title_with_spaces() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/projects/my-project/wiki/My%20Page.json",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(sample_wiki_page_json()),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let project = ProjectIdent::Identifier(
            crate::ids::ProjectIdentifier::from_str("my-project").unwrap(),
        );
        let title = WikiTitle::new("My Page").unwrap();
        client
            .as_user(&cred)
            .get_wiki_page(&project, &title, None, false)
            .await
            .expect("get_wiki_page should succeed");
    }

    #[tokio::test]
    async fn write_wiki_page_issues_only_a_put_no_follow_up_get() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("PUT"))
            .and(wiremock::matchers::path(
                "/projects/my-project/wiki/Old_Title.json",
            ))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "wiki_page": {"text": "body", "title": "New_Title"}
            })))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let project = ProjectIdent::Identifier(
            crate::ids::ProjectIdentifier::from_str("my-project").unwrap(),
        );
        let title = WikiTitle::new("Old_Title").unwrap();
        let write = wiki::WikiPageWrite {
            text: "body".to_string(),
            title: Some("New_Title".to_string()),
            ..Default::default()
        };
        client
            .as_user(&cred)
            .write_wiki_page(&project, &title, &write)
            .await
            .expect("write_wiki_page should succeed");
    }

    #[tokio::test]
    async fn upsert_wiki_page_issues_a_put_then_exactly_one_get() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("PUT"))
            .and(wiremock::matchers::path(
                "/projects/my-project/wiki/Home.json",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/projects/my-project/wiki/Home.json",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(sample_wiki_page_json()),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let project = ProjectIdent::Identifier(
            crate::ids::ProjectIdentifier::from_str("my-project").unwrap(),
        );
        let title = WikiTitle::new("Home").unwrap();
        let write = wiki::WikiPageWrite {
            text: "Welcome".to_string(),
            ..Default::default()
        };
        let page = client
            .as_user(&cred)
            .upsert_wiki_page(&project, &title, &write)
            .await
            .expect("upsert_wiki_page should succeed");
        assert_eq!(page.title, "Home");
    }

    #[tokio::test]
    async fn delete_wiki_page_succeeds_on_200() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("DELETE"))
            .and(wiremock::matchers::path(
                "/projects/my-project/wiki/Home.json",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = discovery_client(&server);
        let cred = Credential::ApiKey(SecretString::from("k"));
        let project = ProjectIdent::Identifier(
            crate::ids::ProjectIdentifier::from_str("my-project").unwrap(),
        );
        let title = WikiTitle::new("Home").unwrap();
        client
            .as_user(&cred)
            .delete_wiki_page(&project, &title)
            .await
            .expect("delete_wiki_page should succeed");
    }
}

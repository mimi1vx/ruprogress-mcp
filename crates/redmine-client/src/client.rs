//! The Redmine HTTP client: builder, credential scoping, and request core.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::auth::Credential;
use crate::error::Error;
use crate::ids::{IssueId, ProjectIdent};
use crate::model::{Collection, issue, project, time_entry, user};
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
    /// never be picked up ambiently.
    #[must_use]
    pub fn as_user<'a>(&'a self, credential: &'a Credential) -> Scoped<'a> {
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
            credential,
        })
    }
}

/// A [`RedmineClient`] scoped to one credential. The only handle that can
/// perform a request.
#[derive(Debug)]
pub struct Scoped<'a> {
    inner: &'a Inner,
    credential: &'a Credential,
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

    pub(crate) async fn put_json<B: Serialize>(&self, path: &str, body: &B) -> crate::Result<()> {
        let url = self.build_url(path, None)?;
        let template = self.credential.apply(self.inner.http.put(url)).json(body);
        self.send_with_retry(&http::Method::PUT, &template).await?;
        Ok(())
    }

    #[allow(
        dead_code,
        reason = "part of the 1.3 request core; no phase-1 API method needs DELETE yet"
    )]
    pub(crate) async fn delete(&self, path: &str) -> crate::Result<()> {
        let url = self.build_url(path, None)?;
        let template = self.credential.apply(self.inner.http.delete(url));
        self.send_with_retry(&http::Method::DELETE, &template)
            .await?;
        Ok(())
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
}

// --- Phase-1 API surface (plan §1.10) ---

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

    /// `PUT /issues/{id}.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Redmine rejects the payload
    /// (e.g. 422 with validation errors).
    pub async fn update_issue(&self, id: IssueId, patch: &issue::IssueUpdate) -> crate::Result<()> {
        self.put_json(
            &format!("issues/{id}.json"),
            &issue::IssueUpdateEnvelope { issue: patch },
        )
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
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
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
}

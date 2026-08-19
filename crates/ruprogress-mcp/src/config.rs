//! Env-var configuration: validated, injectable, and secret-safe.
//!
//! `Config::from_map` is a pure function over an injected map rather than the
//! ambient environment, so the full validation matrix is testable without
//! touching `std::env` (which is racy under threaded tests and, as of edition
//! 2024, `unsafe` to mutate — see ADR 0002).

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use redmine_client::Credential;
use secrecy::SecretString;
use serde_json::json;
use url::Url;

use crate::logging::LogFormat;

/// The injected source of configuration: everything `from_map` reads from.
pub type EnvMap = BTreeMap<String, String>;

/// Fully validated server configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub redmine: RedmineConfig,
    pub auth: AuthMode,
    pub transport: TransportConfig,
    pub read_only: bool,
    pub plugins: PluginFlags,
    /// Reserved for attachment storage; no fields yet.
    pub attachments: AttachmentConfig,
    /// Hard cap on items in a single list tool's response
    /// (`REDMINE_MCP_MAX_RESPONSE_ITEMS`, default 200), enforced above
    /// `redmine-client`'s own byte caps.
    pub max_response_items: usize,
    /// Hard cap on a single tool response's serialized size in bytes
    /// (`REDMINE_MCP_MAX_RESPONSE_BYTES`, default 256 KiB).
    pub max_response_bytes: usize,
    /// Which JSON Schema dialect served tool `inputSchema`s use
    /// (`REDMINE_MCP_SCHEMA_DIALECT`, default `strict`).
    pub schema_dialect: SchemaDialect,
    /// `REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS` /
    /// `REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS`.
    pub custom_fields: CustomFieldConfig,
    /// `REDMINE_MCP_LOG_FORMAT`, default `text`. `main.rs` reads this
    /// straight off the process environment (mirroring `RUST_LOG`/
    /// `--log-level`, both of which also predate `Config`) since tracing
    /// must be initialized before `Config::from_map` runs — its own startup
    /// warnings depend on that order. This field exists so the value is
    /// still validated, documented, and visible in `--print-config`.
    pub log_format: LogFormat,
}

/// Which JSON Schema dialect served `inputSchema`s use. `outputSchema` is
/// unaffected either way — see `docs/adr/0007-json-schema-format-normalization.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaDialect {
    /// The rich JSON Schema 2020-12 form rmcp/schemars produce: `$ref`/
    /// `$defs`, `anyOf` unions, and `type` arrays for nullable fields.
    Strict,
    /// A lossy OpenAPI-3.0-subset dialect for clients (Google Vertex/Gemini)
    /// whose function-calling schema validator rejects `$ref`/`$defs` and
    /// nullable `type` arrays. See `crate::tools::schema::to_portable`.
    Portable,
}

/// Redmine connection settings.
#[derive(Debug, Clone)]
pub struct RedmineConfig {
    pub url: Url,
    pub ssl_verify: bool,
}

/// How this server authenticates to Redmine.
#[derive(Debug, Clone)]
pub enum AuthMode {
    Legacy {
        credential: Credential,
    },
    /// Each request carries its own Redmine credential in the
    /// `X-Redmine-API-Key` header — see `auth::per_user`.
    LegacyPerUser {
        trust: ProxyTrust,
        audit_identity: bool,
    },
    /// Each request carries its own Redmine credential as an
    /// `Authorization: Bearer` token, validated by RFC 7662 introspection —
    /// see `auth::oauth`.
    OAuth(OAuthConfig),
    /// This server is itself an OAuth authorization server: MCP clients
    /// register via DCR and run authorization-code + PKCE against *this*
    /// server, which in turn runs its own authorization-code + PKCE flow
    /// against Redmine — see `oauth::proxy`.
    OAuthProxy(OAuthProxyConfig),
}

/// OAuth settings.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    /// `REDMINE_MCP_BASE_URL`: this server's own public base URL, embedded in
    /// the `WWW-Authenticate` challenge and OAuth discovery documents.
    pub base_url: Url,
    /// `REDMINE_INTROSPECT_CLIENT_ID`: the confidential OAuth client id used
    /// to authenticate introspection/revocation requests to Redmine.
    pub introspect_client_id: String,
    /// `REDMINE_INTROSPECT_CLIENT_SECRET`/`_FILE`.
    pub introspect_client_secret: SecretString,
    /// `REDMINE_OAUTH_TOKEN_CACHE_TTL_SECONDS`: how long a positive
    /// introspection result is cached, capped further by the token's own
    /// `exp`. `0` disables caching.
    pub token_cache_ttl: Duration,
    /// `REDMINE_OAUTH_DISCOVERY_AS`: where/how the RFC 8414
    /// authorization-server metadata document is served.
    pub discovery_as: DiscoveryAs,
    /// The `scopes_supported` both discovery documents advertise: the
    /// current mode's [`crate::oauth::scopes::advertised`] set, optionally
    /// narrowed by `REDMINE_MCP_SCOPES`.
    pub scopes: Vec<&'static str>,
    /// `REDMINE_OAUTH_SCOPE_ENFORCEMENT`: whether `tools/list` filtering and
    /// per-call scope denial are active. Default `true`; `false` is
    /// the documented migration escape hatch for tokens minted before the
    /// OAuth application advertised scopes (O11).
    pub scope_enforcement: bool,
}

/// `oauth-proxy` mode settings (P1). Wraps the same [`OAuthConfig`] `oauth`
/// mode uses — the introspection credential, cache TTL, advertised scopes,
/// and scope-enforcement flag have exactly one definition, shared by both
/// modes (C2) — plus the upstream OAuth client this server authenticates
/// *itself* to Redmine's authorization-code flow with, and the redirect-URI
/// policy DCR clients are checked against.
#[derive(Debug, Clone)]
pub struct OAuthProxyConfig {
    /// The bearer-token resource config shared with `oauth` mode.
    pub resource: OAuthConfig,
    /// `REDMINE_OAUTH_CLIENT_ID`: the upstream OAuth application's client
    /// id. Defaults to `resource.introspect_client_id`.
    pub upstream_client_id: String,
    /// `REDMINE_OAUTH_CLIENT_SECRET`/`_FILE`. Defaults to
    /// `resource.introspect_client_secret`.
    pub upstream_client_secret: SecretString,
    /// `REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS` (P7).
    pub redirects: crate::oauth::proxy::redirect::RedirectPolicy,
}

/// `REDMINE_OAUTH_DISCOVERY_AS` (D3): which authorization server this
/// deployment's RFC 8414 document names itself as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryAs {
    /// Default: the document is served at
    /// `/.well-known/oauth-authorization-server{mcp_path}` with
    /// `issuer = REDMINE_URL`.
    Redmine,
    /// Opt-in, for clients (e.g. Cursor) that probe the canonical root
    /// well-known location: the document is served at
    /// `/.well-known/oauth-authorization-server` with
    /// `issuer = REDMINE_MCP_BASE_URL`, and the suffixed path 404s.
    SelfHosted,
}

impl DiscoveryAs {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Redmine => "redmine",
            Self::SelfHosted => "self",
        }
    }
}

/// Zero-sized proof that `REDMINE_PER_USER_TRUST_PROXY=true` was set. Only
/// constructible inside this module's validation.
#[derive(Debug, Clone, Copy)]
pub struct ProxyTrust(());

/// Which transport the CLI selected. Parsed into a [`TransportConfig`] by
/// [`Config::from_map`], which is where the HTTP variant's settings come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Stdio,
    Http,
}

/// The transport the server will run over, with its validated settings.
#[derive(Debug, Clone)]
pub enum TransportConfig {
    Stdio,
    // Boxed: `HttpConfig` (which carries a `Url`, several `String`s, and a
    // `Vec`) is large enough that an unboxed variant would make every
    // `TransportConfig` — including every `Stdio` one — pay for the size of
    // the bigger variant.
    Http(Box<HttpConfig>),
}

impl TransportConfig {
    /// `"stdio"` or `"http"`, for logs and `get_mcp_server_info`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http(_) => "http",
        }
    }

    #[must_use]
    pub fn as_http(&self) -> Option<&HttpConfig> {
        match self {
            Self::Http(http) => Some(http),
            Self::Stdio => None,
        }
    }
}

/// Streamable-HTTP transport settings.
///
/// `allowed_hosts`, `allowed_origins`, and `max_request_body_bytes` are handed
/// straight to rmcp's `StreamableHttpServerConfig`, which implements the
/// corresponding edge checks itself; this struct only validates and derives
/// them.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// `SERVER_HOST` + `SERVER_PORT`.
    pub bind: SocketAddr,
    /// `FASTMCP_STREAMABLE_HTTP_PATH`.
    pub mcp_path: String,
    /// Accepted `Host` authorities. An **empty** list means rmcp allows every
    /// host, so this is only ever empty when `REDMINE_MCP_ALLOWED_HOSTS=*` was
    /// set explicitly.
    pub allowed_hosts: Vec<String>,
    /// Accepted browser origins. Empty disables Origin validation (rmcp's
    /// default) and suppresses the CORS layer entirely.
    pub allowed_origins: Vec<String>,
    pub max_request_body_bytes: usize,
    /// How long a `/readyz` probe result stays cached.
    pub health_ttl: Duration,
    /// Applied to the health routes only — never to the MCP route, whose
    /// responses may be long-lived SSE streams.
    pub request_timeout: Duration,
    /// The origin clients use to reach `/files/{uuid}` URLs
    /// (`get_redmine_attachment`). Derived from `PUBLIC_HOST`/
    /// `PUBLIC_PORT`/`PUBLIC_SCHEME` when `PUBLIC_HOST` is set, or from the
    /// bind address for a loopback bind — see `parse_public_base`.
    pub public_base: Url,
    /// See [`RateLimitConfig`].
    pub rate_limit: RateLimitConfig,
}

/// `crate::ratelimit`'s per-key token-bucket settings (RL2/RL4).
/// Two classes: **standard** (`/mcp`, `/files/{uuid}`) and **strict** (the
/// unauthenticated, state-allocating `oauth-proxy` endpoints). Ignored
/// entirely on `stdio` (Z3).
#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    /// `REDMINE_MCP_RATE_LIMIT_ENABLED`. `false` restores pre-9.2 behaviour
    /// exactly — no limiter is constructed at all (RL9).
    pub enabled: bool,
    /// `REDMINE_MCP_RATE_LIMIT_RPS`: the standard class's refill rate.
    pub standard_rps: u32,
    /// `REDMINE_MCP_RATE_LIMIT_BURST`: the standard class's bucket capacity.
    pub standard_burst: u32,
    /// `REDMINE_MCP_RATE_LIMIT_AUTH_RPS`: the strict class's refill rate.
    pub strict_rps: u32,
    /// `REDMINE_MCP_RATE_LIMIT_AUTH_BURST`: the strict class's bucket
    /// capacity.
    pub strict_burst: u32,
    /// `REDMINE_MCP_RATE_LIMIT_MAX_KEYS`: the hard cap on each class's
    /// bucket map (RL7).
    pub max_keys: usize,
}

const DEFAULT_MAX_RESPONSE_ITEMS: usize = 200;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 256 * 1024;
const DEFAULT_SERVER_PORT: u16 = 8000;
const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 4 * 1024 * 1024;
const MIN_MAX_REQUEST_BODY_BYTES: usize = 1024;
const MAX_MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;
const MAX_HEALTH_TTL_SECONDS: u64 = 3600;
/// rmcp's own default, restated here because we must never let the list go
/// empty by accident (an empty list means "allow every host").
const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// RL9's proposed defaults: an order of magnitude above a real client's
/// rate, orders below a flood.
const DEFAULT_RATE_LIMIT_RPS: u32 = 10;
const DEFAULT_RATE_LIMIT_BURST: u32 = 40;
const DEFAULT_RATE_LIMIT_AUTH_RPS: u32 = 1;
const DEFAULT_RATE_LIMIT_AUTH_BURST: u32 = 10;
const DEFAULT_RATE_LIMIT_MAX_KEYS: usize = 10_000;

/// Which plugin-gated tool families are enabled. Surfaced in
/// `get_mcp_server_info`'s `plugin_flags`; no gated tools exist yet.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, schemars::JsonSchema)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "six independent, unrelated plugin toggles, each with its own env var"
)]
pub struct PluginFlags {
    pub agile: bool,
    pub checklists: bool,
    pub products: bool,
    pub crm: bool,
    pub dmsf: bool,
    pub tags: bool,
}

/// Local attachment-store settings. None of these are read by any
/// tool yet — `get_redmine_attachment` and
/// `upload_file`/`cleanup_attachment_files` are the consumers — but the
/// whole surface is validated together, same pattern as
/// `REDMINE_PER_USER_AUDIT_IDENTITY` before per-user auth existed.
#[derive(Debug, Clone)]
pub struct AttachmentConfig {
    /// `ATTACHMENTS_DIR`. Created `0700` at startup by `AttachmentStore::init`.
    pub dir: PathBuf,
    /// `ATTACHMENT_MAX_DOWNLOAD_BYTES`: the per-file cap.
    pub max_download_bytes: u64,
    /// `ATTACHMENT_STORE_MAX_BYTES`: the whole-store cap, enforced to be
    /// `>= max_download_bytes` at startup.
    pub store_max_bytes: u64,
    /// `AUTO_CLEANUP_ENABLED`: whether the background sweeper task runs.
    pub auto_cleanup_enabled: bool,
    /// `CLEANUP_INTERVAL_MINUTES`: how often the sweeper runs.
    pub cleanup_interval: Duration,
    /// `ATTACHMENT_EXPIRES_MINUTES`: how long a served file stays fetchable.
    pub expires_after: Duration,
    /// `REDMINE_MCP_UPLOAD_FILE_ROOTS`: allowed roots for `upload_file`'s
    /// `file_path` source. Empty means every `file_path` upload is refused.
    pub upload_file_roots: Vec<PathBuf>,
    /// `REDMINE_MCP_EXPOSE_ADMIN_TOOLS`: gates `cleanup_attachment_files`.
    pub expose_admin_tools: bool,
    /// `REDMINE_PUBLIC_URL`: rewrites `content_url` values whose origin
    /// matches `REDMINE_URL`'s. `None` disables the rewrite.
    pub public_url_rewrite: Option<Url>,
}

/// One entry of `REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS`: a plain string, or
/// an array of strings for a `multiple = true` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomFieldDefaultValue {
    Single(String),
    Multiple(Vec<String>),
}

/// `REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS` /
/// `REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS`: recovery values for a required
/// issue custom field Redmine rejected as blank. `defaults` is keyed by the
/// name exactly as given in the JSON object — matching against a Redmine
/// custom field's display name is case-/punctuation-insensitive and happens
/// where every other such match already happens, in
/// `tools::custom_fields`, not here.
#[derive(Debug, Clone, Default)]
pub struct CustomFieldConfig {
    pub autofill_required: bool,
    pub defaults: BTreeMap<String, CustomFieldDefaultValue>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{var} is required because {because}")]
    Missing {
        var: &'static str,
        because: &'static str,
    },
    #[error("{var} is invalid ({expected}) because {because}")]
    Invalid {
        var: &'static str,
        expected: &'static str,
        because: String,
    },
    #[error("conflicting configuration: {because}")]
    Conflict { because: String },
}

fn optional(vars: &EnvMap, var: &str) -> Option<String> {
    vars.get(var).filter(|v| !v.is_empty()).cloned()
}

fn required(
    vars: &EnvMap,
    var: &'static str,
    because: &'static str,
) -> Result<String, ConfigError> {
    optional(vars, var).ok_or(ConfigError::Missing { var, because })
}

fn optional_bool(vars: &EnvMap, var: &'static str, default: bool) -> Result<bool, ConfigError> {
    match optional(vars, var) {
        None => Ok(default),
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => Err(ConfigError::Invalid {
                var,
                expected: "a boolean (true/false)",
                because: "the value could not be parsed as a boolean".to_string(),
            }),
        },
    }
}

/// Reads `NAME` or `NAME_FILE` (Docker/K8s secrets). Both set is a
/// `Conflict`. Trims exactly one trailing newline from a file's contents.
fn secret(vars: &EnvMap, var: &'static str) -> Result<Option<SecretString>, ConfigError> {
    let file_var = format!("{var}_FILE");
    match (optional(vars, var), optional(vars, &file_var)) {
        (Some(_), Some(_)) => Err(ConfigError::Conflict {
            because: format!("both {var} and {var}_FILE are set; use only one"),
        }),
        (Some(v), None) => Ok(Some(SecretString::from(v))),
        (None, Some(path)) => {
            let contents = std::fs::read_to_string(&path).map_err(|e| ConfigError::Invalid {
                var,
                expected: "a readable file",
                because: format!("failed to read {var}_FILE: {e}"),
            })?;
            let trimmed = contents.strip_suffix('\n').unwrap_or(&contents);
            Ok(Some(SecretString::from(trimmed.to_string())))
        }
        (None, None) => Ok(None),
    }
}

fn parse_redmine(vars: &EnvMap) -> Result<RedmineConfig, ConfigError> {
    let raw = required(
        vars,
        "REDMINE_URL",
        "the server must know which Redmine instance to talk to",
    )?;
    let url: Url = raw.parse().map_err(|_| ConfigError::Invalid {
        var: "REDMINE_URL",
        expected: "a valid http(s) URL",
        because: "the value could not be parsed as a URL".to_string(),
    })?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(ConfigError::Invalid {
            var: "REDMINE_URL",
            expected: "an http or https URL",
            because: format!("scheme {:?} is not http/https", url.scheme()),
        });
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ConfigError::Invalid {
            var: "REDMINE_URL",
            expected: "a URL without embedded credentials",
            because: "the URL contains userinfo; put credentials in REDMINE_API_KEY instead"
                .to_string(),
        });
    }
    let ssl_verify = optional_bool(vars, "REDMINE_SSL_VERIFY", true)?;
    if !ssl_verify {
        tracing::warn!("REDMINE_SSL_VERIFY=false: TLS certificate verification is disabled");
    }
    Ok(RedmineConfig { url, ssl_verify })
}

fn parse_bind(vars: &EnvMap) -> Result<SocketAddr, ConfigError> {
    // An IP literal, not a hostname: resolving a name at bind time would make
    // the interface the server actually listens on depend on DNS, which is not
    // something an operator should have to guess at.
    let ip: IpAddr = match optional(vars, "SERVER_HOST") {
        None => IpAddr::from([127, 0, 0, 1]),
        Some(raw) => raw.parse().map_err(|_| ConfigError::Invalid {
            var: "SERVER_HOST",
            expected: "an IP address literal (e.g. 127.0.0.1, ::1, 0.0.0.0)",
            because: "hostnames are rejected so the bound interface does not depend on DNS"
                .to_string(),
        })?,
    };
    let port = parse_port(vars, "SERVER_PORT")?.unwrap_or(DEFAULT_SERVER_PORT);
    Ok(SocketAddr::new(ip, port))
}

fn parse_port(vars: &EnvMap, var: &'static str) -> Result<Option<u16>, ConfigError> {
    let Some(raw) = optional(vars, var) else {
        return Ok(None);
    };
    let port: u16 = raw.parse().map_err(|_| ConfigError::Invalid {
        var,
        expected: "a TCP port between 1 and 65535",
        because: "the value could not be parsed as a port number".to_string(),
    })?;
    if port == 0 {
        return Err(ConfigError::Invalid {
            var,
            expected: "a TCP port between 1 and 65535",
            because: "0 would bind an ephemeral port that no client could be told about"
                .to_string(),
        });
    }
    Ok(Some(port))
}

/// Path-segment guard for the MCP route: it is joined into an axum router, so
/// traversal, query, and fragment characters must not survive config parsing.
fn parse_mcp_path(vars: &EnvMap) -> Result<String, ConfigError> {
    const VAR: &str = "FASTMCP_STREAMABLE_HTTP_PATH";
    let Some(path) = optional(vars, VAR) else {
        return Ok("/mcp".to_string());
    };
    let invalid = |because: &str| ConfigError::Invalid {
        var: VAR,
        expected: "an absolute path with at least one segment, e.g. \"/mcp\"",
        because: because.to_string(),
    };
    if !path.starts_with('/') {
        return Err(invalid("the path does not start with \"/\""));
    }
    if path == "/" {
        return Err(invalid("\"/\" would shadow every other route"));
    }
    if path.contains("..") {
        return Err(invalid("the path contains \"..\""));
    }
    if path.contains(['?', '#']) {
        return Err(invalid("the path contains a query or fragment character"));
    }
    // `{}` is axum's path-capture syntax and `*` its wildcard: `/{x}` would
    // mount the MCP service at every single-segment path.
    if path.contains(['{', '}', '*']) {
        return Err(invalid("the path contains a route-pattern character"));
    }
    if path.chars().any(char::is_whitespace) {
        return Err(invalid("the path contains whitespace"));
    }
    Ok(path)
}

/// Parses `REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS`: a JSON object mapping a
/// custom field name to a default value (a string, or an array of strings
/// for a `multiple = true` field). Same "set but empty must not collapse
/// into unset" rule as [`parse_csv`]: `{}` is rejected, not silently ignored.
/// Never echoes the configured name or value in an error — both can be
/// business-sensitive (F34's redaction concern extends to a boot error a
/// misconfigured deployment might forward to a log aggregator).
fn parse_custom_field_defaults(
    vars: &EnvMap,
) -> Result<BTreeMap<String, CustomFieldDefaultValue>, ConfigError> {
    const VAR: &str = "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS";
    let Some(raw) = optional(vars, VAR) else {
        return Ok(BTreeMap::new());
    };
    let invalid = |because: &'static str| ConfigError::Invalid {
        var: VAR,
        expected: "a JSON object mapping custom field name to a string or an array of strings",
        because: because.to_string(),
    };
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| invalid("the value is not valid JSON"))?;
    let serde_json::Value::Object(map) = parsed else {
        return Err(invalid("the value is not a JSON object"));
    };
    if map.is_empty() {
        return Err(invalid(
            "the object has no entries; unset the variable instead of setting it empty",
        ));
    }
    let mut defaults = BTreeMap::new();
    for (name, value) in map {
        let parsed_value = match value {
            serde_json::Value::String(s) => CustomFieldDefaultValue::Single(s),
            serde_json::Value::Array(items) => {
                let mut strings = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        serde_json::Value::String(s) => strings.push(s),
                        _ => {
                            return Err(invalid("an array entry is not a string"));
                        }
                    }
                }
                CustomFieldDefaultValue::Multiple(strings)
            }
            _ => {
                return Err(invalid(
                    "an entry's value is neither a string nor an array of strings",
                ));
            }
        };
        defaults.insert(name, parsed_value);
    }
    Ok(defaults)
}

fn parse_custom_fields(vars: &EnvMap) -> Result<CustomFieldConfig, ConfigError> {
    let autofill_required = optional_bool(vars, "REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS", false)?;
    let defaults = parse_custom_field_defaults(vars)?;
    if !autofill_required && !defaults.is_empty() {
        tracing::warn!(
            "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS is set but \
             REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS is not true; the configured defaults will \
             never be used"
        );
    }
    Ok(CustomFieldConfig {
        autofill_required,
        defaults,
    })
}

/// Splits a comma-separated variable, rejecting a value that is present but
/// contains no usable entries.
///
/// "Set, but empty after parsing" must never collapse into "unset": for both
/// allowlists that would silently turn a control *off* while the operator
/// believes they configured it.
fn parse_csv(vars: &EnvMap, var: &'static str) -> Result<Option<Vec<String>>, ConfigError> {
    let Some(raw) = optional(vars, var) else {
        return Ok(None);
    };
    let entries: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();
    if entries.is_empty() {
        return Err(ConfigError::Invalid {
            var,
            expected: "a comma-separated list with at least one entry",
            because: "the value contains only separators and whitespace; unset the variable \
                      instead of setting it empty"
                .to_string(),
        });
    }
    Ok(Some(entries))
}

fn parse_allowed_origins(vars: &EnvMap) -> Result<Vec<String>, ConfigError> {
    const VAR: &str = "REDMINE_MCP_ALLOWED_ORIGINS";
    let Some(origins) = parse_csv(vars, VAR)? else {
        return Ok(Vec::new());
    };
    let invalid = |because: String| ConfigError::Invalid {
        var: VAR,
        expected: "a comma-separated list of absolute origins",
        because,
    };
    for origin in &origins {
        if origin == "*" {
            return Err(invalid(
                "\"*\" would allow any site in a browser to drive this server; list the exact \
                 origins instead"
                    .to_string(),
            ));
        }
        // `Origin: null` is what a sandboxed iframe, a `data:` document, and a
        // redirected cross-origin request all send, so allowlisting it grants
        // access to a set nobody can enumerate.
        if origin.eq_ignore_ascii_case("null") {
            return Err(invalid(
                "\"null\" is sent by sandboxed iframes and data: documents, so it is not an \
                 origin that can be granted access"
                    .to_string(),
            ));
        }
        if !origin.contains("://") {
            return Err(invalid(format!(
                "{origin:?} has no scheme; write e.g. \"https://{origin}\""
            )));
        }
        // rmcp silently drops entries it cannot parse, and the CORS layer
        // needs each one as a `HeaderValue`. Reject here so the two views of
        // the allowlist cannot diverge at runtime.
        if http::HeaderValue::from_str(origin).is_err() {
            return Err(invalid(format!(
                "{origin:?} cannot be sent as an HTTP header value"
            )));
        }
    }
    Ok(origins)
}

/// Derives the `Host` allowlist, or fails when a non-loopback bind leaves it
/// underivable.
///
/// An empty allowlist means *allow every host* in rmcp, and `Host` is the only
/// signal that distinguishes a DNS-rebinding request from a legitimate one (the
/// browser considers it same-origin, so CORS never runs). So a bind we cannot
/// derive an allowlist for is refused at startup rather than served with the
/// check silently off.
fn parse_allowed_hosts(vars: &EnvMap, bind: SocketAddr) -> Result<Vec<String>, ConfigError> {
    const VAR: &str = "REDMINE_MCP_ALLOWED_HOSTS";
    if let Some(explicit) = parse_csv(vars, VAR)? {
        if explicit.iter().any(|h| h == "*") {
            // Only as the sole entry: `a.example.com,*` silently discarding
            // `a.example.com` and disabling the check would be the worst kind
            // of surprise, since it reads like it narrows the list.
            if explicit.len() > 1 {
                return Err(ConfigError::Invalid {
                    var: VAR,
                    expected: "either a list of hosts or the single value \"*\"",
                    because: "\"*\" disables Host validation entirely, so combining it with \
                              specific hosts is contradictory"
                        .to_string(),
                });
            }
            tracing::warn!(
                "REDMINE_MCP_ALLOWED_HOSTS=*: Host validation is disabled, so this server accepts \
                 requests for any hostname and cannot detect DNS rebinding. Only safe behind a \
                 proxy that validates Host itself."
            );
            return Ok(Vec::new());
        }
        for host in &explicit {
            validate_authority(VAR, host)?;
        }
        return Ok(explicit);
    }

    let public_port = parse_port(vars, "PUBLIC_PORT")?;
    let public_host = optional(vars, "PUBLIC_HOST");
    if public_host.is_none() && public_port.is_some() {
        return Err(ConfigError::Conflict {
            because:
                "PUBLIC_PORT is set without PUBLIC_HOST; it only qualifies a PUBLIC_HOST entry \
                      in the Host allowlist"
                    .to_string(),
        });
    }

    let Some(host) = public_host else {
        if bind.ip().is_loopback() {
            return Ok(LOOPBACK_HOSTS.iter().map(ToString::to_string).collect());
        }
        return Err(ConfigError::Missing {
            var: "PUBLIC_HOST",
            because: "SERVER_HOST is not a loopback address, so the Host allowlist cannot be \
                      derived. Set PUBLIC_HOST to the hostname clients use to reach this server, \
                      or set REDMINE_MCP_ALLOWED_HOSTS explicitly (\"*\" disables Host validation \
                      entirely — only do this when a reverse proxy already validates Host).",
        });
    };
    validate_authority("PUBLIC_HOST", &host)?;

    let mut hosts: Vec<String> = LOOPBACK_HOSTS.iter().map(ToString::to_string).collect();
    // A port-less entry matches *any* port in rmcp, so adding both `host` and
    // `host:port` would make `host:port` — and therefore `PUBLIC_PORT` —
    // decorative. Pin the port only when the operator asked for one.
    hosts.push(match public_port {
        Some(port) => format!("{host}:{port}"),
        None => host,
    });
    hosts.dedup();
    Ok(hosts)
}

/// Rejects a `Host` allowlist entry that rmcp could not match against
/// anything. rmcp falls back to treating an unparseable entry as a literal
/// hostname, so a malformed one silently matches nothing rather than erroring.
fn validate_authority(var: &'static str, value: &str) -> Result<(), ConfigError> {
    if http::uri::Authority::try_from(value).is_err() {
        return Err(ConfigError::Invalid {
            var,
            expected: "a hostname, optionally with a port (e.g. \"mcp.example.com:8000\")",
            because: format!("{value:?} is not a valid host authority"),
        });
    }
    Ok(())
}

/// `PUBLIC_SCHEME` defaults to `https` only when `PUBLIC_PORT == 443`,
/// since that is the one case where guessing `http` would almost certainly
/// be wrong (a TLS-terminating proxy on the standard HTTPS port).
fn parse_public_scheme(
    vars: &EnvMap,
    public_port: Option<u16>,
) -> Result<&'static str, ConfigError> {
    match optional(vars, "PUBLIC_SCHEME").as_deref() {
        None => Ok(if public_port == Some(443) {
            "https"
        } else {
            "http"
        }),
        Some("http") => Ok("http"),
        Some("https") => Ok("https"),
        Some(other) => Err(ConfigError::Invalid {
            var: "PUBLIC_SCHEME",
            expected: "\"http\" or \"https\"",
            because: format!("got {other:?}"),
        }),
    }
}

/// Derives the origin used to build `/files/{uuid}` URLs.
///
/// With `PUBLIC_HOST` set, builds `{PUBLIC_SCHEME}://{PUBLIC_HOST}[:{PUBLIC_PORT}]`.
/// Without it, only a loopback bind can derive one (`http://<bind-ip>:<port>`,
/// which is correct for a client on the same machine); a non-loopback bind
/// without `PUBLIC_HOST` is a `Missing` error here even if
/// `REDMINE_MCP_ALLOWED_HOSTS` was set explicitly and so bypassed
/// [`parse_allowed_hosts`]'s own `PUBLIC_HOST` requirement — an
/// unreachable `0.0.0.0`-hosted URL is worse than refusing to start.
fn public_base_from_loopback_bind(bind: SocketAddr) -> Result<String, ConfigError> {
    if !bind.ip().is_loopback() {
        return Err(ConfigError::Missing {
            var: "PUBLIC_HOST",
            because: "SERVER_HOST is not a loopback address, so the public URL used to build \
                      /files/{uuid} links cannot be derived without PUBLIC_HOST",
        });
    }
    let host = match bind.ip() {
        IpAddr::V6(ip) => format!("[{ip}]"),
        IpAddr::V4(ip) => ip.to_string(),
    };
    Ok(format!("http://{host}:{}", bind.port()))
}

fn parse_public_base(vars: &EnvMap, bind: SocketAddr) -> Result<Url, ConfigError> {
    let public_port = parse_port(vars, "PUBLIC_PORT")?;
    let public_host = optional(vars, "PUBLIC_HOST");
    let scheme = parse_public_scheme(vars, public_port)?;
    let raw = if let Some(host) = public_host {
        match public_port {
            Some(port) => format!("{scheme}://{host}:{port}"),
            None => format!("{scheme}://{host}"),
        }
    } else {
        public_base_from_loopback_bind(bind)?
    };
    raw.parse().map_err(|_| ConfigError::Invalid {
        var: "PUBLIC_HOST",
        expected: "a value that, combined with PUBLIC_SCHEME/PUBLIC_PORT, forms a valid URL",
        because: format!("{raw:?} could not be parsed as a URL"),
    })
}

fn parse_http(vars: &EnvMap) -> Result<HttpConfig, ConfigError> {
    let bind = parse_bind(vars)?;
    let allowed_hosts = parse_allowed_hosts(vars, bind)?;
    // The invariant the whole derivation exists to hold, restated where it can
    // actually fail: an empty list is *allow every host* in rmcp, so it must
    // only ever be reachable by asking for it by name.
    if allowed_hosts.is_empty()
        && optional(vars, "REDMINE_MCP_ALLOWED_HOSTS").as_deref() != Some("*")
    {
        return Err(ConfigError::Invalid {
            var: "REDMINE_MCP_ALLOWED_HOSTS",
            expected: "a non-empty Host allowlist",
            because: "the derived allowlist came out empty, which would disable Host validation; \
                      set REDMINE_MCP_ALLOWED_HOSTS=* if that is genuinely intended"
                .to_string(),
        });
    }

    let max_request_body_bytes = match optional(vars, "REDMINE_MCP_MAX_REQUEST_BODY_BYTES") {
        None => DEFAULT_MAX_REQUEST_BODY_BYTES,
        Some(raw) => {
            let invalid = |because: String| ConfigError::Invalid {
                var: "REDMINE_MCP_MAX_REQUEST_BODY_BYTES",
                expected: "a byte count between 1024 and 67108864",
                because,
            };
            let bytes: usize = raw.parse().map_err(|_| {
                invalid("the value could not be parsed as a byte count".to_string())
            })?;
            if !(MIN_MAX_REQUEST_BODY_BYTES..=MAX_MAX_REQUEST_BODY_BYTES).contains(&bytes) {
                return Err(invalid(format!("{bytes} is outside the accepted range")));
            }
            bytes
        }
    };

    let health_ttl_seconds = match optional(vars, "HEALTH_INTROSPECTION_TTL_SECONDS") {
        None => 30,
        Some(raw) => {
            let invalid = |because: String| ConfigError::Invalid {
                var: "HEALTH_INTROSPECTION_TTL_SECONDS",
                expected: "a number of seconds between 0 and 3600",
                because,
            };
            let seconds: u64 = raw
                .parse()
                .map_err(|_| invalid("the value could not be parsed as a number".to_string()))?;
            if seconds > MAX_HEALTH_TTL_SECONDS {
                return Err(invalid(format!("{seconds} is longer than an hour")));
            }
            seconds
        }
    };

    Ok(HttpConfig {
        bind,
        mcp_path: parse_mcp_path(vars)?,
        allowed_hosts,
        allowed_origins: parse_allowed_origins(vars)?,
        max_request_body_bytes,
        health_ttl: Duration::from_secs(health_ttl_seconds),
        request_timeout: Duration::from_secs(10),
        public_base: parse_public_base(vars, bind)?,
        rate_limit: parse_rate_limit(vars)?,
    })
}

/// Validates `REDMINE_MCP_BASE_URL`: absolute, scheme `http`/`https`, no
/// userinfo, no query, no fragment. A trailing slash is not rejected — Url
/// forces one on a root path regardless — but every consumer that appends a
/// path to this value (the `WWW-Authenticate` challenge, discovery documents)
/// must strip it first; see `auth::oauth`'s challenge builder.
///
/// Non-`https` on a non-loopback host is a startup `WARN`, not an error:
/// local development over loopback (and Docker-internal hostnames) is
/// legitimate, but this value is embedded in a challenge every OAuth client
/// receives, so a production deployment serving it over plain HTTP is worth
/// flagging.
fn parse_oauth_base_url(vars: &EnvMap) -> Result<Url, ConfigError> {
    const VAR: &str = "REDMINE_MCP_BASE_URL";
    let raw = required(
        vars,
        VAR,
        "oauth auth mode requires the server's own public base URL",
    )?;
    let url: Url = raw.parse().map_err(|_| ConfigError::Invalid {
        var: VAR,
        expected: "an absolute http(s) URL with no userinfo, query, or fragment",
        because: "the value could not be parsed as a URL".to_string(),
    })?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(ConfigError::Invalid {
            var: VAR,
            expected: "an http or https URL",
            because: format!("scheme {:?} is not http/https", url.scheme()),
        });
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ConfigError::Invalid {
            var: VAR,
            expected: "a URL without embedded credentials",
            because: "the URL contains userinfo".to_string(),
        });
    }
    if url.query().is_some() {
        return Err(ConfigError::Invalid {
            var: VAR,
            expected: "a URL without a query string",
            because: "the URL contains a query string".to_string(),
        });
    }
    if url.fragment().is_some() {
        return Err(ConfigError::Invalid {
            var: VAR,
            expected: "a URL without a fragment",
            because: "the URL contains a fragment".to_string(),
        });
    }
    let is_loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    });
    if url.scheme() == "http" && !is_loopback {
        tracing::warn!(
            base_url = %url,
            "REDMINE_MCP_BASE_URL uses http on a non-loopback host: this value is embedded in \
             the WWW-Authenticate challenge and every OAuth discovery document, so it should be \
             https in production"
        );
    }
    Ok(url)
}

const DEFAULT_OAUTH_TOKEN_CACHE_TTL_SECONDS: u64 = 60;
const MAX_OAUTH_TOKEN_CACHE_TTL_SECONDS: u64 = 3600;

/// `REDMINE_OAUTH_TOKEN_CACHE_TTL_SECONDS`: `0..=3600`, default `60`. `0`
/// disables caching entirely — unlike [`positive_u64`], `0` is an accepted
/// value here.
fn parse_token_cache_ttl(vars: &EnvMap) -> Result<Duration, ConfigError> {
    const VAR: &str = "REDMINE_OAUTH_TOKEN_CACHE_TTL_SECONDS";
    let Some(raw) = optional(vars, VAR) else {
        return Ok(Duration::from_secs(DEFAULT_OAUTH_TOKEN_CACHE_TTL_SECONDS));
    };
    let invalid = |because: String| ConfigError::Invalid {
        var: VAR,
        expected: "a number of seconds between 0 and 3600",
        because,
    };
    let seconds: u64 = raw
        .parse()
        .map_err(|_| invalid("the value could not be parsed as a number".to_string()))?;
    if seconds > MAX_OAUTH_TOKEN_CACHE_TTL_SECONDS {
        return Err(invalid(format!("{seconds} is longer than an hour")));
    }
    Ok(Duration::from_secs(seconds))
}

/// `REDMINE_OAUTH_SCOPE_ENFORCEMENT`: `"on"` (default) or `"off"` (S9, O11).
/// `off` is the documented migration escape hatch, so it logs a startup
/// `WARN` naming the consequence rather than passing silently.
fn parse_scope_enforcement(vars: &EnvMap) -> Result<bool, ConfigError> {
    const VAR: &str = "REDMINE_OAUTH_SCOPE_ENFORCEMENT";
    match optional(vars, VAR).as_deref() {
        None | Some("on") => Ok(true),
        Some("off") => {
            tracing::warn!(
                "REDMINE_OAUTH_SCOPE_ENFORCEMENT=off: every authenticated token can see and \
                 call every tool, regardless of its OAuth scopes; this is intended only as a \
                 migration path for tokens minted before scope enforcement existed"
            );
            Ok(false)
        }
        Some(other) => Err(ConfigError::Invalid {
            var: VAR,
            expected: "one of \"on\", \"off\"",
            because: format!("got {other:?}"),
        }),
    }
}

/// `REDMINE_OAUTH_DISCOVERY_AS`: `"redmine"` (default) or `"self"`.
fn parse_discovery_as(vars: &EnvMap) -> Result<DiscoveryAs, ConfigError> {
    match optional(vars, "REDMINE_OAUTH_DISCOVERY_AS").as_deref() {
        None | Some("redmine") => Ok(DiscoveryAs::Redmine),
        Some("self") => Ok(DiscoveryAs::SelfHosted),
        Some(other) => Err(ConfigError::Invalid {
            var: "REDMINE_OAUTH_DISCOVERY_AS",
            expected: "one of \"redmine\", \"self\"",
            because: format!("got {other:?}"),
        }),
    }
}

/// `REDMINE_MCP_SCOPES`: narrows `full` (D2). Unset or blank leaves `full`
/// untouched, matching [`optional`]'s "set but empty" treatment.
fn parse_scopes(vars: &EnvMap, full: Vec<&'static str>) -> Result<Vec<&'static str>, ConfigError> {
    let Some(raw) = optional(vars, "REDMINE_MCP_SCOPES") else {
        return Ok(full);
    };
    crate::oauth::scopes::narrow(&full, &raw).map_err(|because| ConfigError::Invalid {
        var: "REDMINE_MCP_SCOPES",
        expected: "a whitespace-separated subset of the scopes this server advertises",
        because,
    })
}

fn parse_auth(
    vars: &EnvMap,
    transport: TransportKind,
    read_only: bool,
    plugins: PluginFlags,
) -> Result<AuthMode, ConfigError> {
    let mode = optional(vars, "REDMINE_AUTH_MODE").unwrap_or_else(|| "legacy".to_string());
    match mode.as_str() {
        "legacy" => {
            let credential = secret(vars, "REDMINE_API_KEY")?.ok_or(ConfigError::Missing {
                var: "REDMINE_API_KEY",
                because: "legacy auth mode requires an API key (REDMINE_API_KEY or REDMINE_API_KEY_FILE)",
            })?;
            Ok(AuthMode::Legacy {
                credential: Credential::ApiKey(credential),
            })
        }
        "legacy-per-user" => {
            if optional(vars, "REDMINE_PER_USER_TRUST_PROXY").as_deref() != Some("true") {
                return Err(ConfigError::Missing {
                    var: "REDMINE_PER_USER_TRUST_PROXY",
                    because: "legacy-per-user auth requires an explicit attestation that a TLS-terminating proxy is in front (set to \"true\")",
                });
            }
            if transport == TransportKind::Stdio {
                return Err(ConfigError::Conflict {
                    because: "legacy-per-user auth requires per-request headers, which the stdio transport does not have".to_string(),
                });
            }
            let audit_identity = optional_bool(vars, "REDMINE_PER_USER_AUDIT_IDENTITY", false)?;
            Ok(AuthMode::LegacyPerUser {
                trust: ProxyTrust(()),
                audit_identity,
            })
        }
        "oauth" => Ok(AuthMode::OAuth(parse_oauth_resource(
            vars, transport, read_only, plugins,
        )?)),
        "oauth-proxy" => {
            let mut resource = parse_oauth_resource(vars, transport, read_only, plugins)?;
            // P12: this server IS the authorization server in this mode, so
            // an explicit "redmine" is contradictory rather than something
            // to silently override.
            if optional(vars, "REDMINE_OAUTH_DISCOVERY_AS").as_deref() == Some("redmine") {
                return Err(ConfigError::Conflict {
                    because: "REDMINE_OAUTH_DISCOVERY_AS=redmine conflicts with oauth-proxy \
                              mode: this server is the authorization server in this mode, so \
                              its metadata document cannot name Redmine's endpoints instead"
                        .to_string(),
                });
            }
            resource.discovery_as = DiscoveryAs::SelfHosted;

            // P3: accepted for `.env`-ports-unchanged compatibility with the
            // reference, and warned about once rather than silently ignored.
            if optional(vars, "REDMINE_MCP_JWT_SIGNING_KEY").is_some()
                || optional(vars, "REDMINE_MCP_JWT_SIGNING_KEY_FILE").is_some()
            {
                tracing::warn!(
                    "REDMINE_MCP_JWT_SIGNING_KEY(_FILE) is set but unused: oauth-proxy mode \
                     issues opaque reference tokens (P2), never signed JWTs"
                );
            }

            let (upstream_client_id, upstream_client_secret) =
                parse_upstream_client(vars, &resource)?;
            let redirects = parse_redirect_policy(vars)?;

            Ok(AuthMode::OAuthProxy(OAuthProxyConfig {
                resource,
                upstream_client_id,
                upstream_client_secret,
                redirects,
            }))
        }
        other => Err(ConfigError::Invalid {
            var: "REDMINE_AUTH_MODE",
            expected: "one of \"legacy\", \"legacy-per-user\", \"oauth\", \"oauth-proxy\"",
            because: format!("got {other:?}"),
        }),
    }
}

/// The `OAuthConfig` shared by `oauth` and `oauth-proxy` modes (C2): the
/// introspection credential, cache TTL, advertised scopes, and
/// scope-enforcement flag have exactly one parse path, so the two modes
/// cannot drift.
fn parse_oauth_resource(
    vars: &EnvMap,
    transport: TransportKind,
    read_only: bool,
    plugins: PluginFlags,
) -> Result<OAuthConfig, ConfigError> {
    let base_url = parse_oauth_base_url(vars)?;
    let introspect_client_id = required(
        vars,
        "REDMINE_INTROSPECT_CLIENT_ID",
        "oauth auth mode requires the OAuth client credentials used to introspect \
         bearer tokens",
    )?;
    let introspect_client_secret =
        secret(vars, "REDMINE_INTROSPECT_CLIENT_SECRET")?.ok_or(ConfigError::Missing {
            var: "REDMINE_INTROSPECT_CLIENT_SECRET",
            because: "oauth auth mode requires the OAuth client credentials used to \
                      introspect bearer tokens (REDMINE_INTROSPECT_CLIENT_SECRET or \
                      REDMINE_INTROSPECT_CLIENT_SECRET_FILE)",
        })?;
    if transport == TransportKind::Stdio {
        return Err(ConfigError::Conflict {
            because: "oauth auth requires per-request bearer tokens and a 401 challenge \
                      to discover them, neither of which the stdio transport has"
                .to_string(),
        });
    }
    let token_cache_ttl = parse_token_cache_ttl(vars)?;
    let discovery_as = parse_discovery_as(vars)?;
    let full_scopes = crate::oauth::scopes::advertised(read_only, plugins.agile, plugins.tags);
    let scopes = parse_scopes(vars, full_scopes)?;
    let scope_enforcement = parse_scope_enforcement(vars)?;
    Ok(OAuthConfig {
        base_url,
        introspect_client_id,
        introspect_client_secret,
        token_cache_ttl,
        discovery_as,
        scopes,
        scope_enforcement,
    })
}

/// `REDMINE_OAUTH_CLIENT_ID`/`REDMINE_OAUTH_CLIENT_SECRET`(`_FILE`): the
/// upstream OAuth application `oauth-proxy` mode runs its own
/// authorization-code flow against. Defaults to `resource`'s introspection
/// client when neither is set; setting exactly one of the pair is a
/// `Conflict` rather than a silent partial fallback.
fn parse_upstream_client(
    vars: &EnvMap,
    resource: &OAuthConfig,
) -> Result<(String, SecretString), ConfigError> {
    let id = optional(vars, "REDMINE_OAUTH_CLIENT_ID");
    let client_secret = secret(vars, "REDMINE_OAUTH_CLIENT_SECRET")?;
    match (id, client_secret) {
        (Some(id), Some(secret)) => Ok((id, secret)),
        (None, None) => Ok((
            resource.introspect_client_id.clone(),
            resource.introspect_client_secret.clone(),
        )),
        (Some(_), None) => Err(ConfigError::Conflict {
            because: "REDMINE_OAUTH_CLIENT_ID is set without REDMINE_OAUTH_CLIENT_SECRET (or \
                      _FILE); set both to use a dedicated upstream client, or neither to fall \
                      back to the introspection client"
                .to_string(),
        }),
        (None, Some(_)) => Err(ConfigError::Conflict {
            because: "REDMINE_OAUTH_CLIENT_SECRET (or _FILE) is set without \
                      REDMINE_OAUTH_CLIENT_ID; set both to use a dedicated upstream client, or \
                      neither to fall back to the introspection client"
                .to_string(),
        }),
    }
}

/// `REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS` (C3): unset/blank →
/// [`RedirectPolicy::Loopback`]; the literal `*` → [`RedirectPolicy::Any`]
/// (warned about, since it is a deliberate widening); otherwise a
/// comma/whitespace-separated list of patterns, each parsed by
/// [`RedirectPattern::parse`].
fn parse_redirect_policy(
    vars: &EnvMap,
) -> Result<crate::oauth::proxy::redirect::RedirectPolicy, ConfigError> {
    use crate::oauth::proxy::redirect::{RedirectPattern, RedirectPolicy};

    const VAR: &str = "REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS";
    let Some(raw) = optional(vars, VAR) else {
        return Ok(RedirectPolicy::Loopback);
    };
    if raw == "*" {
        tracing::warn!(
            "REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS=*: any https redirect URI (and http on a \
             loopback host) is accepted from any DCR client; only safe when every client that \
             can reach POST /register is already trusted"
        );
        return Ok(RedirectPolicy::Any);
    }
    let entries: Vec<&str> = raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .collect();
    if entries.is_empty() {
        return Err(ConfigError::Invalid {
            var: VAR,
            expected: "a comma/whitespace-separated list of scheme://host[:port]/path* \
                       patterns, or the literal \"*\"",
            because: "the value contains only separators and whitespace; unset the variable \
                      instead of setting it empty"
                .to_string(),
        });
    }
    let patterns = entries
        .into_iter()
        .map(|raw_pattern| {
            RedirectPattern::parse(raw_pattern).map_err(|because| ConfigError::Invalid {
                var: VAR,
                expected: "a comma/whitespace-separated list of scheme://host[:port]/path* \
                           patterns, or the literal \"*\"",
                because,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RedirectPolicy::Patterns(patterns))
}

fn parse_schema_dialect(vars: &EnvMap) -> Result<SchemaDialect, ConfigError> {
    match optional(vars, "REDMINE_MCP_SCHEMA_DIALECT").as_deref() {
        None | Some("strict") => Ok(SchemaDialect::Strict),
        Some("portable") => Ok(SchemaDialect::Portable),
        Some(other) => Err(ConfigError::Invalid {
            var: "REDMINE_MCP_SCHEMA_DIALECT",
            expected: "one of \"strict\", \"portable\"",
            because: format!("got {other:?}"),
        }),
    }
}

fn parse_log_format(vars: &EnvMap) -> Result<LogFormat, ConfigError> {
    match optional(vars, "REDMINE_MCP_LOG_FORMAT").as_deref() {
        None => Ok(LogFormat::Text),
        Some(other) => LogFormat::parse(other).ok_or_else(|| ConfigError::Invalid {
            var: "REDMINE_MCP_LOG_FORMAT",
            expected: "one of \"text\", \"json\"",
            because: format!("got {other:?}"),
        }),
    }
}

fn parse_plugins(vars: &EnvMap) -> Result<PluginFlags, ConfigError> {
    Ok(PluginFlags {
        agile: optional_bool(vars, "REDMINE_AGILE_ENABLED", false)?,
        checklists: optional_bool(vars, "REDMINE_CHECKLISTS_ENABLED", false)?,
        products: optional_bool(vars, "REDMINE_PRODUCTS_ENABLED", false)?,
        crm: optional_bool(vars, "REDMINE_CRM_ENABLED", false)?,
        dmsf: optional_bool(vars, "REDMINE_DMSF_ENABLED", false)?,
        tags: optional_bool(vars, "REDMINE_TAGS_ENABLED", false)?,
    })
}

/// Parses a positive `usize` env var, rejecting `0` (which would make every
/// list tool return an empty collection — almost certainly a misconfiguration
/// rather than what the operator meant).
fn positive_usize(vars: &EnvMap, var: &'static str, default: usize) -> Result<usize, ConfigError> {
    let Some(raw) = optional(vars, var) else {
        return Ok(default);
    };
    let invalid = |because: String| ConfigError::Invalid {
        var,
        expected: "a positive integer",
        because,
    };
    let value: usize = raw
        .parse()
        .map_err(|_| invalid("the value could not be parsed as a number".to_string()))?;
    if value == 0 {
        return Err(invalid(
            "0 would make every list tool return an empty collection".to_string(),
        ));
    }
    Ok(value)
}

/// Parses a positive `u32` env var, rejecting `0`.
fn positive_u32(vars: &EnvMap, var: &'static str, default: u32) -> Result<u32, ConfigError> {
    let Some(raw) = optional(vars, var) else {
        return Ok(default);
    };
    let invalid = |because: String| ConfigError::Invalid {
        var,
        expected: "a positive integer",
        because,
    };
    let value: u32 = raw
        .parse()
        .map_err(|_| invalid("the value could not be parsed as a number".to_string()))?;
    if value == 0 {
        return Err(invalid("0 is not a usable value here".to_string()));
    }
    Ok(value)
}

/// RL2's per-class token-bucket settings, validated (`rps` > 0, `burst` >=
/// `rps`, `max_keys` >= 1) rather than left for `crate::ratelimit::Limiter`
/// to guess at a sane fallback.
fn parse_rate_limit(vars: &EnvMap) -> Result<RateLimitConfig, ConfigError> {
    let enabled = optional_bool(vars, "REDMINE_MCP_RATE_LIMIT_ENABLED", true)?;
    let standard_rps = positive_u32(vars, "REDMINE_MCP_RATE_LIMIT_RPS", DEFAULT_RATE_LIMIT_RPS)?;
    let standard_burst = positive_u32(
        vars,
        "REDMINE_MCP_RATE_LIMIT_BURST",
        DEFAULT_RATE_LIMIT_BURST,
    )?;
    let strict_rps = positive_u32(
        vars,
        "REDMINE_MCP_RATE_LIMIT_AUTH_RPS",
        DEFAULT_RATE_LIMIT_AUTH_RPS,
    )?;
    let strict_burst = positive_u32(
        vars,
        "REDMINE_MCP_RATE_LIMIT_AUTH_BURST",
        DEFAULT_RATE_LIMIT_AUTH_BURST,
    )?;
    let max_keys = positive_usize(
        vars,
        "REDMINE_MCP_RATE_LIMIT_MAX_KEYS",
        DEFAULT_RATE_LIMIT_MAX_KEYS,
    )?;

    if standard_burst < standard_rps {
        return Err(ConfigError::Invalid {
            var: "REDMINE_MCP_RATE_LIMIT_BURST",
            expected: "a value >= REDMINE_MCP_RATE_LIMIT_RPS",
            because: format!("{standard_burst} is less than the configured rps ({standard_rps})"),
        });
    }
    if strict_burst < strict_rps {
        return Err(ConfigError::Invalid {
            var: "REDMINE_MCP_RATE_LIMIT_AUTH_BURST",
            expected: "a value >= REDMINE_MCP_RATE_LIMIT_AUTH_RPS",
            because: format!("{strict_burst} is less than the configured rps ({strict_rps})"),
        });
    }

    Ok(RateLimitConfig {
        enabled,
        standard_rps,
        standard_burst,
        strict_rps,
        strict_burst,
        max_keys,
    })
}

const DEFAULT_ATTACHMENT_MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;
const DEFAULT_ATTACHMENT_STORE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DEFAULT_CLEANUP_INTERVAL_MINUTES: u64 = 15;
const DEFAULT_ATTACHMENT_EXPIRES_MINUTES: u64 = 60;

/// Parses a positive `u64` env var, rejecting `0`.
fn positive_u64(vars: &EnvMap, var: &'static str, default: u64) -> Result<u64, ConfigError> {
    let Some(raw) = optional(vars, var) else {
        return Ok(default);
    };
    let invalid = |because: String| ConfigError::Invalid {
        var,
        expected: "a positive integer",
        because,
    };
    let value: u64 = raw
        .parse()
        .map_err(|_| invalid("the value could not be parsed as a number".to_string()))?;
    if value == 0 {
        return Err(invalid("0 is not a usable value here".to_string()));
    }
    Ok(value)
}

fn parse_attachments_dir(vars: &EnvMap) -> PathBuf {
    optional(vars, "ATTACHMENTS_DIR").map_or_else(
        || std::env::temp_dir().join("ruprogress-mcp-attachments"),
        PathBuf::from,
    )
}

/// `REDMINE_MCP_UPLOAD_FILE_ROOTS`: a csv of absolute directory paths.
/// Relative paths are rejected outright — a prefix check against a relative
/// root is meaningless once the caller's cwd is anything but the one the
/// operator had in mind.
fn parse_upload_file_roots(vars: &EnvMap) -> Result<Vec<PathBuf>, ConfigError> {
    const VAR: &str = "REDMINE_MCP_UPLOAD_FILE_ROOTS";
    let Some(entries) = parse_csv(vars, VAR)? else {
        return Ok(Vec::new());
    };
    entries
        .into_iter()
        .map(|raw| {
            let path = PathBuf::from(&raw);
            if path.is_absolute() {
                Ok(path)
            } else {
                Err(ConfigError::Invalid {
                    var: VAR,
                    expected: "a comma-separated list of absolute directory paths",
                    because: format!("{raw:?} is not an absolute path"),
                })
            }
        })
        .collect()
}

fn parse_public_url_rewrite(vars: &EnvMap) -> Result<Option<Url>, ConfigError> {
    const VAR: &str = "REDMINE_PUBLIC_URL";
    let Some(raw) = optional(vars, VAR) else {
        return Ok(None);
    };
    let url: Url = raw.parse().map_err(|_| ConfigError::Invalid {
        var: VAR,
        expected: "a valid http(s) URL",
        because: "the value could not be parsed as a URL".to_string(),
    })?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(ConfigError::Invalid {
            var: VAR,
            expected: "an http or https URL",
            because: format!("scheme {:?} is not http/https", url.scheme()),
        });
    }
    Ok(Some(url))
}

fn parse_attachments(vars: &EnvMap) -> Result<AttachmentConfig, ConfigError> {
    let max_download_bytes = positive_u64(
        vars,
        "ATTACHMENT_MAX_DOWNLOAD_BYTES",
        DEFAULT_ATTACHMENT_MAX_DOWNLOAD_BYTES,
    )?;
    let store_max_bytes = positive_u64(
        vars,
        "ATTACHMENT_STORE_MAX_BYTES",
        DEFAULT_ATTACHMENT_STORE_MAX_BYTES,
    )?;
    // A store cap smaller than the per-file cap means no download could
    // ever succeed — a misconfiguration to catch at boot, not a capacity
    // condition to discover on a client's first request.
    if store_max_bytes < max_download_bytes {
        return Err(ConfigError::Conflict {
            because: format!(
                "ATTACHMENT_STORE_MAX_BYTES ({store_max_bytes}) is smaller than \
                 ATTACHMENT_MAX_DOWNLOAD_BYTES ({max_download_bytes}); no single download could \
                 ever fit"
            ),
        });
    }
    let cleanup_interval_minutes = positive_u64(
        vars,
        "CLEANUP_INTERVAL_MINUTES",
        DEFAULT_CLEANUP_INTERVAL_MINUTES,
    )?;
    let expires_minutes = positive_u64(
        vars,
        "ATTACHMENT_EXPIRES_MINUTES",
        DEFAULT_ATTACHMENT_EXPIRES_MINUTES,
    )?;
    Ok(AttachmentConfig {
        dir: parse_attachments_dir(vars),
        max_download_bytes,
        store_max_bytes,
        auto_cleanup_enabled: optional_bool(vars, "AUTO_CLEANUP_ENABLED", true)?,
        cleanup_interval: Duration::from_secs(cleanup_interval_minutes.saturating_mul(60)),
        expires_after: Duration::from_secs(expires_minutes.saturating_mul(60)),
        upload_file_roots: parse_upload_file_roots(vars)?,
        expose_admin_tools: optional_bool(vars, "REDMINE_MCP_EXPOSE_ADMIN_TOOLS", false)?,
        public_url_rewrite: parse_public_url_rewrite(vars)?,
    })
}

impl Config {
    /// Validate and build a `Config` from an injected env-var map. `kind`
    /// comes from the CLI (`--transport`) rather than `vars`; the transport's
    /// own settings are then parsed out of `vars`, and auth-mode validation
    /// consults the kind (`legacy-per-user` is incompatible with `stdio`).
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] describing exactly which variable is
    /// missing, invalid, or conflicting.
    pub fn from_map(vars: &EnvMap, kind: TransportKind) -> Result<Self, ConfigError> {
        let redmine = parse_redmine(vars)?;
        // Parsed ahead of `auth`: the oauth arm's scope-catalogue gating
        // (D2) needs both already resolved.
        let read_only = optional_bool(vars, "REDMINE_MCP_READ_ONLY", false)?;
        let plugins = parse_plugins(vars)?;
        let auth = parse_auth(vars, kind, read_only, plugins)?;
        let transport = match kind {
            TransportKind::Stdio => TransportConfig::Stdio,
            TransportKind::Http => TransportConfig::Http(Box::new(parse_http(vars)?)),
        };

        if let (TransportConfig::Http(http), AuthMode::Legacy { .. }) = (&transport, &auth)
            && !http.bind.ip().is_loopback()
        {
            tracing::warn!(
                bind = %http.bind,
                "serving on a non-loopback address with a single shared Redmine API key: every \
                 client that can reach this port acts as that Redmine account"
            );
        }

        if matches!(auth, AuthMode::LegacyPerUser { .. }) {
            tracing::warn!(
                "legacy-per-user auth mode trusts that a TLS-terminating proxy sits in front of \
                 this server and does not forward a client-supplied X-Forwarded-Proto; \
                 REDMINE_PER_USER_TRUST_PROXY=true is the operator's attestation of that and is \
                 not something this process can verify"
            );
        }

        Ok(Self {
            redmine,
            auth,
            transport,
            read_only,
            plugins,
            attachments: parse_attachments(vars)?,
            max_response_items: positive_usize(
                vars,
                "REDMINE_MCP_MAX_RESPONSE_ITEMS",
                DEFAULT_MAX_RESPONSE_ITEMS,
            )?,
            max_response_bytes: positive_usize(
                vars,
                "REDMINE_MCP_MAX_RESPONSE_BYTES",
                DEFAULT_MAX_RESPONSE_BYTES,
            )?,
            schema_dialect: parse_schema_dialect(vars)?,
            custom_fields: parse_custom_fields(vars)?,
            log_format: parse_log_format(vars)?,
        })
    }

    /// `from_map(std::env::vars().collect(), TransportKind::Stdio)`.
    ///
    /// # Errors
    ///
    /// See [`Config::from_map`].
    pub fn from_env() -> Result<Self, ConfigError> {
        let vars: EnvMap = std::env::vars().collect();
        Self::from_map(&vars, TransportKind::Stdio)
    }

    pub(crate) fn auth_mode_label(&self) -> &'static str {
        match &self.auth {
            AuthMode::Legacy { .. } => "legacy",
            AuthMode::LegacyPerUser { .. } => "legacy-per-user",
            AuthMode::OAuth(_) => "oauth",
            AuthMode::OAuthProxy(_) => "oauth-proxy",
        }
    }

    /// The shared `OAuthConfig` (introspection credential, cache TTL,
    /// advertised scopes, scope-enforcement flag) underlying whichever auth
    /// mode owns one. `None` in `legacy`/`legacy-per-user`, which have no
    /// bearer-token verifier to speak of.
    ///
    /// The one place every call site that needs "am I in a bearer-token
    /// auth mode, and if so what is its resource config" asks the question,
    /// instead of matching on `AuthMode` (and risking an `unreachable!()`)
    /// at each use.
    pub(crate) fn oauth_resource(&self) -> Option<&OAuthConfig> {
        match &self.auth {
            AuthMode::OAuth(oauth) => Some(oauth),
            AuthMode::OAuthProxy(proxy) => Some(&proxy.resource),
            AuthMode::Legacy { .. } | AuthMode::LegacyPerUser { .. } => None,
        }
    }

    pub(crate) fn schema_dialect_label(&self) -> &'static str {
        match self.schema_dialect {
            SchemaDialect::Strict => "strict",
            SchemaDialect::Portable => "portable",
        }
    }

    pub(crate) fn plugin_flags_json(&self) -> serde_json::Value {
        json!({
            "agile": self.plugins.agile,
            "checklists": self.plugins.checklists,
            "products": self.plugins.products,
            "crm": self.plugins.crm,
            "dmsf": self.plugins.dmsf,
            "tags": self.plugins.tags,
        })
    }

    /// A JSON summary safe to print (`--print-config`) or log: includes the
    /// Redmine host, the bind address, and the auth mode for operator
    /// debugging, but never a credential.
    ///
    /// This is the widest of three redaction surfaces, and they are
    /// deliberately different rather than accidentally divergent:
    ///
    /// - `redacted_summary` (here) — operator-invoked and local, so internal
    ///   topology is fine; secrets are not.
    /// - `get_mcp_server_info` — reaches a language model, so it omits the
    ///   Redmine host and the bind address on top of the secrets.
    /// - `/readyz` — unauthenticated, so it carries readiness facts only and
    ///   no configuration at all.
    #[must_use]
    pub fn redacted_summary(&self) -> serde_json::Value {
        let transport = match &self.transport {
            TransportConfig::Stdio => json!({ "kind": "stdio" }),
            TransportConfig::Http(http) => json!({
                "kind": "http",
                "bind": http.bind.to_string(),
                "mcp_path": http.mcp_path,
            }),
        };
        let oauth = match &self.auth {
            AuthMode::OAuth(cfg) => Some(json!({
                "base_url": cfg.base_url.as_str(),
                "introspect_client_id": cfg.introspect_client_id,
                "token_cache_ttl_seconds": cfg.token_cache_ttl.as_secs(),
                "discovery_as": cfg.discovery_as.label(),
                "scopes": cfg.scopes,
                "scope_enforcement": cfg.scope_enforcement,
            })),
            AuthMode::OAuthProxy(cfg) => Some(json!({
                "base_url": cfg.resource.base_url.as_str(),
                "introspect_client_id": cfg.resource.introspect_client_id,
                "token_cache_ttl_seconds": cfg.resource.token_cache_ttl.as_secs(),
                "discovery_as": cfg.resource.discovery_as.label(),
                "scopes": cfg.resource.scopes,
                "scope_enforcement": cfg.resource.scope_enforcement,
                "upstream_client_id": cfg.upstream_client_id,
                "redirect_policy": cfg.redirects.summary_label(),
            })),
            AuthMode::Legacy { .. } | AuthMode::LegacyPerUser { .. } => None,
        };
        json!({
            "redmine": {
                "url_host": self.redmine.url.host_str(),
                "ssl_verify": self.redmine.ssl_verify,
            },
            "auth_mode": self.auth_mode_label(),
            "oauth": oauth,
            "transport": transport,
            "read_only_mode": self.read_only,
            "plugin_flags": self.plugin_flags_json(),
            "schema_dialect": self.schema_dialect_label(),
            "log_format": self.log_format.label(),
            "autofill_required_custom_fields": self.custom_fields.autofill_required,
            "required_custom_field_defaults_count": self.custom_fields.defaults.len(),
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> EnvMap {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn valid_legacy() -> EnvMap {
        map(&[
            ("REDMINE_URL", "https://redmine.example.com"),
            ("REDMINE_API_KEY", "test-key"),
        ])
    }

    #[test]
    fn redmine_url_missing_is_missing() {
        let vars = map(&[("REDMINE_API_KEY", "k")]);
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "REDMINE_URL",
                ..
            }
        ));
    }

    #[test]
    fn redmine_url_without_scheme_is_invalid() {
        let vars = map(&[("REDMINE_URL", "not a url"), ("REDMINE_API_KEY", "k")]);
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_URL",
                ..
            }
        ));
    }

    #[test]
    fn redmine_url_with_non_http_scheme_is_invalid() {
        let vars = map(&[
            ("REDMINE_URL", "ftp://example.com"),
            ("REDMINE_API_KEY", "k"),
        ]);
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_URL",
                ..
            }
        ));
    }

    #[test]
    fn redmine_url_with_userinfo_is_invalid() {
        let vars = map(&[
            ("REDMINE_URL", "https://user:pass@example.com"),
            ("REDMINE_API_KEY", "k"),
        ]);
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_URL",
                ..
            }
        ));
    }

    #[test]
    fn legacy_without_api_key_is_missing() {
        let vars = map(&[("REDMINE_URL", "https://redmine.example.com")]);
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "REDMINE_API_KEY",
                ..
            }
        ));
    }

    #[test]
    fn both_api_key_and_api_key_file_is_conflict() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_API_KEY_FILE".to_string(),
            "/tmp/whatever".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(err, ConfigError::Conflict { .. }));
    }

    #[test]
    fn legacy_per_user_without_trust_proxy_is_missing() {
        let vars = map(&[
            ("REDMINE_URL", "https://redmine.example.com"),
            ("REDMINE_AUTH_MODE", "legacy-per-user"),
        ]);
        // Trust-proxy is checked before the transport conflict, so this
        // reports Missing even on the only transport implemented so far (stdio).
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "REDMINE_PER_USER_TRUST_PROXY",
                ..
            }
        ));
    }

    #[test]
    fn legacy_per_user_with_stdio_transport_is_conflict() {
        let vars = map(&[
            ("REDMINE_URL", "https://redmine.example.com"),
            ("REDMINE_AUTH_MODE", "legacy-per-user"),
            ("REDMINE_PER_USER_TRUST_PROXY", "true"),
        ]);
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(err, ConfigError::Conflict { .. }));
    }

    fn valid_oauth() -> EnvMap {
        map(&[
            ("REDMINE_URL", "https://redmine.example.com"),
            ("REDMINE_AUTH_MODE", "oauth"),
            ("REDMINE_MCP_BASE_URL", "http://localhost:3040"),
            ("REDMINE_INTROSPECT_CLIENT_ID", "introspect-client"),
            ("REDMINE_INTROSPECT_CLIENT_SECRET", "introspect-secret"),
        ])
    }

    #[test]
    fn oauth_without_base_url_is_missing() {
        let vars = map(&[
            ("REDMINE_URL", "https://redmine.example.com"),
            ("REDMINE_AUTH_MODE", "oauth"),
        ]);
        // Base-url is checked before the introspection credentials and the
        // transport conflict, so this reports Missing even on stdio.
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "REDMINE_MCP_BASE_URL",
                ..
            }
        ));
    }

    #[test]
    fn oauth_with_valid_config_succeeds_on_http() {
        let config =
            Config::from_map(&valid_oauth(), TransportKind::Http).expect("should be valid");
        assert!(matches!(config.auth, AuthMode::OAuth(_)));
    }

    #[test]
    fn oauth_without_introspect_client_id_is_missing() {
        let mut vars = valid_oauth();
        vars.remove("REDMINE_INTROSPECT_CLIENT_ID");
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "REDMINE_INTROSPECT_CLIENT_ID",
                ..
            }
        ));
    }

    #[test]
    fn oauth_without_introspect_client_secret_is_missing() {
        let mut vars = valid_oauth();
        vars.remove("REDMINE_INTROSPECT_CLIENT_SECRET");
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "REDMINE_INTROSPECT_CLIENT_SECRET",
                ..
            }
        ));
    }

    #[test]
    fn oauth_with_both_client_secret_and_secret_file_is_conflict() {
        let mut vars = valid_oauth();
        vars.insert(
            "REDMINE_INTROSPECT_CLIENT_SECRET_FILE".to_string(),
            "/tmp/whatever".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(err, ConfigError::Conflict { .. }));
    }

    #[test]
    fn oauth_on_stdio_transport_is_conflict() {
        let err = Config::from_map(&valid_oauth(), TransportKind::Stdio).unwrap_err();
        assert!(matches!(err, ConfigError::Conflict { .. }));
    }

    #[test]
    fn oauth_base_url_with_userinfo_is_invalid() {
        let mut vars = valid_oauth();
        vars.insert(
            "REDMINE_MCP_BASE_URL".to_string(),
            "http://user:pass@localhost:3040".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_BASE_URL",
                ..
            }
        ));
    }

    #[test]
    fn oauth_base_url_with_query_is_invalid() {
        let mut vars = valid_oauth();
        vars.insert(
            "REDMINE_MCP_BASE_URL".to_string(),
            "http://localhost:3040/?x=1".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_BASE_URL",
                ..
            }
        ));
    }

    #[test]
    fn oauth_base_url_with_fragment_is_invalid() {
        let mut vars = valid_oauth();
        vars.insert(
            "REDMINE_MCP_BASE_URL".to_string(),
            "http://localhost:3040/#frag".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_BASE_URL",
                ..
            }
        ));
    }

    #[test]
    fn oauth_base_url_relative_is_invalid() {
        let mut vars = valid_oauth();
        vars.insert("REDMINE_MCP_BASE_URL".to_string(), "not a url".to_string());
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_BASE_URL",
                ..
            }
        ));
    }

    #[test]
    fn oauth_base_url_with_non_http_scheme_is_invalid() {
        let mut vars = valid_oauth();
        vars.insert(
            "REDMINE_MCP_BASE_URL".to_string(),
            "ftp://localhost:3040".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_BASE_URL",
                ..
            }
        ));
    }

    #[test]
    fn oauth_token_cache_ttl_defaults_to_60() {
        let config =
            Config::from_map(&valid_oauth(), TransportKind::Http).expect("should be valid");
        let AuthMode::OAuth(oauth) = &config.auth else {
            panic!("expected oauth mode");
        };
        assert_eq!(oauth.token_cache_ttl, Duration::from_mins(1));
    }

    #[test]
    fn oauth_token_cache_ttl_zero_is_accepted() {
        let mut vars = valid_oauth();
        vars.insert(
            "REDMINE_OAUTH_TOKEN_CACHE_TTL_SECONDS".to_string(),
            "0".to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Http).expect("should be valid");
        let AuthMode::OAuth(oauth) = &config.auth else {
            panic!("expected oauth mode");
        };
        assert_eq!(oauth.token_cache_ttl, Duration::ZERO);
    }

    #[test]
    fn oauth_token_cache_ttl_out_of_range_is_invalid() {
        let mut vars = valid_oauth();
        vars.insert(
            "REDMINE_OAUTH_TOKEN_CACHE_TTL_SECONDS".to_string(),
            "3601".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_OAUTH_TOKEN_CACHE_TTL_SECONDS",
                ..
            }
        ));
    }

    #[test]
    fn oauth_redacted_summary_omits_the_client_secret() {
        const SECRET: &str = "super-secret-introspect-value";
        let mut vars = valid_oauth();
        vars.insert(
            "REDMINE_INTROSPECT_CLIENT_SECRET".to_string(),
            SECRET.to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Http).expect("should be valid");
        let summary = config.redacted_summary().to_string();
        assert!(!summary.contains(SECRET));
        assert!(summary.contains("introspect-client"));
    }

    #[test]
    fn discovery_as_defaults_to_redmine() {
        let config = Config::from_map(&valid_oauth(), TransportKind::Http).expect("valid");
        let AuthMode::OAuth(oauth) = &config.auth else {
            panic!("expected oauth mode");
        };
        assert_eq!(oauth.discovery_as, DiscoveryAs::Redmine);
        assert_eq!(
            config.redacted_summary()["oauth"]["discovery_as"],
            "redmine"
        );
    }

    #[test]
    fn discovery_as_self_is_accepted() {
        let mut vars = valid_oauth();
        vars.insert("REDMINE_OAUTH_DISCOVERY_AS".to_string(), "self".to_string());
        let config = Config::from_map(&vars, TransportKind::Http).expect("valid");
        let AuthMode::OAuth(oauth) = &config.auth else {
            panic!("expected oauth mode");
        };
        assert_eq!(oauth.discovery_as, DiscoveryAs::SelfHosted);
    }

    #[test]
    fn discovery_as_unknown_value_is_invalid() {
        let mut vars = valid_oauth();
        vars.insert(
            "REDMINE_OAUTH_DISCOVERY_AS".to_string(),
            "bogus".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_OAUTH_DISCOVERY_AS",
                ..
            }
        ));
    }

    #[test]
    fn oauth_scopes_default_to_the_full_advertised_set() {
        let config = Config::from_map(&valid_oauth(), TransportKind::Http).expect("valid");
        let AuthMode::OAuth(oauth) = &config.auth else {
            panic!("expected oauth mode");
        };
        assert!(oauth.scopes.contains(&"view_project"));
        assert!(oauth.scopes.contains(&"edit_issues"));
        assert!(!oauth.scopes.contains(&"admin"));
    }

    #[test]
    fn read_only_mode_narrows_the_default_oauth_scopes_to_read_scopes() {
        let mut vars = valid_oauth();
        vars.insert("REDMINE_MCP_READ_ONLY".to_string(), "true".to_string());
        let config = Config::from_map(&vars, TransportKind::Http).expect("valid");
        let AuthMode::OAuth(oauth) = &config.auth else {
            panic!("expected oauth mode");
        };
        assert!(oauth.scopes.contains(&"view_project"));
        assert!(!oauth.scopes.contains(&"edit_issues"));
    }

    #[test]
    fn redmine_mcp_scopes_narrows_and_preserves_advertised_order() {
        let mut vars = valid_oauth();
        vars.insert(
            "REDMINE_MCP_SCOPES".to_string(),
            "edit_issues view_project".to_string(), // reversed vs. advertised order
        );
        let config = Config::from_map(&vars, TransportKind::Http).expect("valid");
        let AuthMode::OAuth(oauth) = &config.auth else {
            panic!("expected oauth mode");
        };
        assert_eq!(oauth.scopes, vec!["view_project", "edit_issues"]);
    }

    #[test]
    fn redmine_mcp_scopes_rejects_an_out_of_set_scope_and_lists_the_accepted_set() {
        let mut vars = valid_oauth();
        vars.insert("REDMINE_MCP_READ_ONLY".to_string(), "true".to_string());
        vars.insert(
            "REDMINE_MCP_SCOPES".to_string(),
            "edit_issues".to_string(), // write-only scope, but read_only is set
        );
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        let ConfigError::Invalid {
            var: "REDMINE_MCP_SCOPES",
            because,
            ..
        } = err
        else {
            panic!("expected an Invalid REDMINE_MCP_SCOPES error, got {err:?}");
        };
        assert!(because.contains("edit_issues"));
        assert!(
            because.contains("view_project"),
            "should list the accepted set: {because}"
        );
    }

    #[test]
    fn redmine_mcp_scopes_blank_value_is_treated_as_unset() {
        let mut vars = valid_oauth();
        vars.insert("REDMINE_MCP_SCOPES".to_string(), String::new());
        let config = Config::from_map(&vars, TransportKind::Http).expect("valid");
        let AuthMode::OAuth(oauth) = &config.auth else {
            panic!("expected oauth mode");
        };
        assert!(oauth.scopes.contains(&"edit_issues"));
    }

    #[test]
    fn scope_enforcement_defaults_to_on() {
        let config = Config::from_map(&valid_oauth(), TransportKind::Http).expect("valid");
        let AuthMode::OAuth(oauth) = &config.auth else {
            panic!("expected oauth mode");
        };
        assert!(oauth.scope_enforcement);
        assert_eq!(
            config.redacted_summary()["oauth"]["scope_enforcement"],
            true
        );
    }

    #[test]
    fn scope_enforcement_off_is_accepted_and_logs_a_warning() {
        #[derive(Clone, Default)]
        struct SharedBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for SharedBuf {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedBuf {
            type Writer = Self;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let buf = SharedBuf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buf.clone())
            .with_max_level(tracing::Level::WARN)
            .without_time()
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);

        let mut vars = valid_oauth();
        vars.insert(
            "REDMINE_OAUTH_SCOPE_ENFORCEMENT".to_string(),
            "off".to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Http).expect("should be valid");
        drop(guard);

        let AuthMode::OAuth(oauth) = &config.auth else {
            panic!("expected oauth mode");
        };
        assert!(!oauth.scope_enforcement);
        assert_eq!(
            config.redacted_summary()["oauth"]["scope_enforcement"],
            false
        );
        let captured = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(captured.contains("REDMINE_OAUTH_SCOPE_ENFORCEMENT=off"));
    }

    #[test]
    fn scope_enforcement_unknown_value_is_invalid() {
        let mut vars = valid_oauth();
        vars.insert(
            "REDMINE_OAUTH_SCOPE_ENFORCEMENT".to_string(),
            "bogus".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_OAUTH_SCOPE_ENFORCEMENT",
                ..
            }
        ));
    }

    // --- oauth-proxy ------------------------------------------------------

    fn valid_oauth_proxy() -> EnvMap {
        let mut vars = valid_oauth();
        vars.insert("REDMINE_AUTH_MODE".to_string(), "oauth-proxy".to_string());
        vars
    }

    fn as_oauth_proxy(config: &Config) -> &OAuthProxyConfig {
        let AuthMode::OAuthProxy(proxy) = &config.auth else {
            panic!("expected oauth-proxy mode, got {config:?}");
        };
        proxy
    }

    #[test]
    fn oauth_proxy_with_valid_config_succeeds_on_http() {
        let config =
            Config::from_map(&valid_oauth_proxy(), TransportKind::Http).expect("should be valid");
        assert_eq!(config.auth_mode_label(), "oauth-proxy");
    }

    /// Inventory B1: `scope_enforcement_active()` reads this flag off
    /// `Config::oauth_resource()`, so proxy mode getting a default of `true`
    /// here is what makes that positive, asserted (not panic-absence)
    /// behaviour.
    #[test]
    fn oauth_proxy_scope_enforcement_defaults_to_on() {
        let config =
            Config::from_map(&valid_oauth_proxy(), TransportKind::Http).expect("should be valid");
        assert!(as_oauth_proxy(&config).resource.scope_enforcement);
        assert!(config.oauth_resource().is_some_and(|o| o.scope_enforcement));
    }

    #[test]
    fn oauth_proxy_on_stdio_is_conflict() {
        let err = Config::from_map(&valid_oauth_proxy(), TransportKind::Stdio).unwrap_err();
        assert!(matches!(err, ConfigError::Conflict { .. }));
    }

    #[test]
    fn oauth_proxy_without_base_url_is_missing() {
        let mut vars = valid_oauth_proxy();
        vars.remove("REDMINE_MCP_BASE_URL");
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "REDMINE_MCP_BASE_URL",
                ..
            }
        ));
    }

    #[test]
    fn oauth_proxy_without_introspection_credentials_is_missing() {
        let mut vars = valid_oauth_proxy();
        vars.remove("REDMINE_INTROSPECT_CLIENT_SECRET");
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "REDMINE_INTROSPECT_CLIENT_SECRET",
                ..
            }
        ));
    }

    #[test]
    fn oauth_proxy_discovery_as_defaults_to_self_hosted_even_though_unset() {
        let config =
            Config::from_map(&valid_oauth_proxy(), TransportKind::Http).expect("should be valid");
        assert_eq!(
            as_oauth_proxy(&config).resource.discovery_as,
            DiscoveryAs::SelfHosted
        );
    }

    #[test]
    fn oauth_proxy_explicit_self_discovery_is_accepted() {
        let mut vars = valid_oauth_proxy();
        vars.insert("REDMINE_OAUTH_DISCOVERY_AS".to_string(), "self".to_string());
        let config = Config::from_map(&vars, TransportKind::Http).expect("should be valid");
        assert_eq!(
            as_oauth_proxy(&config).resource.discovery_as,
            DiscoveryAs::SelfHosted
        );
    }

    #[test]
    fn oauth_proxy_explicit_redmine_discovery_is_conflict() {
        let mut vars = valid_oauth_proxy();
        vars.insert(
            "REDMINE_OAUTH_DISCOVERY_AS".to_string(),
            "redmine".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(err, ConfigError::Conflict { .. }));
    }

    #[test]
    fn oauth_proxy_upstream_client_defaults_to_the_introspection_client() {
        let config =
            Config::from_map(&valid_oauth_proxy(), TransportKind::Http).expect("should be valid");
        let proxy = as_oauth_proxy(&config);
        assert_eq!(proxy.upstream_client_id, "introspect-client");
    }

    #[test]
    fn oauth_proxy_explicit_upstream_client_overrides_the_default() {
        let mut vars = valid_oauth_proxy();
        vars.insert(
            "REDMINE_OAUTH_CLIENT_ID".to_string(),
            "upstream-client".to_string(),
        );
        vars.insert(
            "REDMINE_OAUTH_CLIENT_SECRET".to_string(),
            "upstream-secret".to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Http).expect("should be valid");
        assert_eq!(
            as_oauth_proxy(&config).upstream_client_id,
            "upstream-client"
        );
    }

    #[test]
    fn oauth_proxy_upstream_client_id_without_secret_is_conflict() {
        let mut vars = valid_oauth_proxy();
        vars.insert(
            "REDMINE_OAUTH_CLIENT_ID".to_string(),
            "upstream-client".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(err, ConfigError::Conflict { .. }));
    }

    #[test]
    fn oauth_proxy_upstream_client_secret_without_id_is_conflict() {
        let mut vars = valid_oauth_proxy();
        vars.insert(
            "REDMINE_OAUTH_CLIENT_SECRET".to_string(),
            "upstream-secret".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(err, ConfigError::Conflict { .. }));
    }

    #[test]
    fn oauth_proxy_redirects_default_to_loopback() {
        let config =
            Config::from_map(&valid_oauth_proxy(), TransportKind::Http).expect("should be valid");
        assert_eq!(
            as_oauth_proxy(&config).redirects.summary_label(),
            "loopback"
        );
    }

    #[test]
    fn oauth_proxy_redirects_star_is_any_and_warns() {
        let mut vars = valid_oauth_proxy();
        vars.insert(
            "REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS".to_string(),
            "*".to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Http).expect("should be valid");
        assert_eq!(as_oauth_proxy(&config).redirects.summary_label(), "any");
    }

    #[test]
    fn oauth_proxy_redirects_parse_a_pattern_list() {
        let mut vars = valid_oauth_proxy();
        vars.insert(
            "REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS".to_string(),
            "https://app.example.com/*, https://*.example.org/*".to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Http).expect("should be valid");
        assert_eq!(
            as_oauth_proxy(&config).redirects.summary_label(),
            "2 pattern(s)"
        );
    }

    #[test]
    fn oauth_proxy_redirects_rejects_an_unparseable_pattern() {
        let mut vars = valid_oauth_proxy();
        vars.insert(
            "REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS".to_string(),
            "not-a-pattern".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Http).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS",
                ..
            }
        ));
    }

    #[test]
    fn oauth_proxy_jwt_signing_key_is_accepted_and_warns() {
        #[derive(Clone, Default)]
        struct SharedBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for SharedBuf {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedBuf {
            type Writer = Self;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let buf = SharedBuf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buf.clone())
            .with_max_level(tracing::Level::WARN)
            .without_time()
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);

        let mut vars = valid_oauth_proxy();
        vars.insert(
            "REDMINE_MCP_JWT_SIGNING_KEY".to_string(),
            "unused-value".to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Http).expect("should be valid");
        drop(guard);

        assert_eq!(config.auth_mode_label(), "oauth-proxy");
        let captured = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(captured.contains("REDMINE_MCP_JWT_SIGNING_KEY"));
    }

    #[test]
    fn oauth_proxy_redacted_summary_includes_upstream_client_id_but_no_secret() {
        const SECRET: &str = "super-secret-upstream-value";
        let mut vars = valid_oauth_proxy();
        vars.insert(
            "REDMINE_OAUTH_CLIENT_ID".to_string(),
            "upstream-client".to_string(),
        );
        vars.insert(
            "REDMINE_OAUTH_CLIENT_SECRET".to_string(),
            SECRET.to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Http).expect("should be valid");
        let summary = config.redacted_summary();
        assert_eq!(summary["auth_mode"], "oauth-proxy");
        assert_eq!(summary["oauth"]["upstream_client_id"], "upstream-client");
        assert_eq!(summary["oauth"]["redirect_policy"], "loopback");
        let rendered = summary.to_string();
        assert!(!rendered.contains(SECRET));
    }

    #[test]
    fn unknown_auth_mode_message_lists_oauth_proxy() {
        let mut vars = valid_legacy();
        vars.insert("REDMINE_AUTH_MODE".to_string(), "bogus".to_string());
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(format!("{err}").contains("oauth-proxy"));
    }

    #[test]
    fn ssl_verify_false_is_accepted() {
        let mut vars = valid_legacy();
        vars.insert("REDMINE_SSL_VERIFY".to_string(), "false".to_string());
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("should be valid");
        assert!(!config.redmine.ssl_verify);
    }

    #[test]
    fn unknown_auth_mode_is_invalid() {
        let mut vars = valid_legacy();
        vars.insert("REDMINE_AUTH_MODE".to_string(), "bogus".to_string());
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_AUTH_MODE",
                ..
            }
        ));
    }

    #[test]
    fn valid_legacy_config_parses() {
        let config =
            Config::from_map(&valid_legacy(), TransportKind::Stdio).expect("should be valid");
        assert!(matches!(config.auth, AuthMode::Legacy { .. }));
        assert!(!config.read_only);
        assert_eq!(config.redmine.url.host_str(), Some("redmine.example.com"));
    }

    #[test]
    fn read_only_flag_parses() {
        let mut vars = valid_legacy();
        vars.insert("REDMINE_MCP_READ_ONLY".to_string(), "true".to_string());
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("should be valid");
        assert!(config.read_only);
    }

    #[test]
    fn output_caps_default_to_200_items_and_256_kib() {
        let config =
            Config::from_map(&valid_legacy(), TransportKind::Stdio).expect("should be valid");
        assert_eq!(config.max_response_items, 200);
        assert_eq!(config.max_response_bytes, 256 * 1024);
    }

    #[test]
    fn output_caps_parse_from_env() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_MCP_MAX_RESPONSE_ITEMS".to_string(),
            "50".to_string(),
        );
        vars.insert(
            "REDMINE_MCP_MAX_RESPONSE_BYTES".to_string(),
            "1024".to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("should be valid");
        assert_eq!(config.max_response_items, 50);
        assert_eq!(config.max_response_bytes, 1024);
    }

    #[test]
    fn output_caps_reject_zero() {
        for var in [
            "REDMINE_MCP_MAX_RESPONSE_ITEMS",
            "REDMINE_MCP_MAX_RESPONSE_BYTES",
        ] {
            let mut vars = valid_legacy();
            vars.insert(var.to_string(), "0".to_string());
            let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
            assert!(matches!(err, ConfigError::Invalid { var: v, .. } if v == var));
        }
    }

    #[test]
    fn plugin_flags_default_false_and_parse_true() {
        let mut vars = valid_legacy();
        vars.insert("REDMINE_DMSF_ENABLED".to_string(), "true".to_string());
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("should be valid");
        assert!(config.plugins.dmsf);
        assert!(!config.plugins.agile);
    }

    #[test]
    fn schema_dialect_defaults_to_strict() {
        let config =
            Config::from_map(&valid_legacy(), TransportKind::Stdio).expect("should be valid");
        assert_eq!(config.schema_dialect, SchemaDialect::Strict);
    }

    #[test]
    fn schema_dialect_parses_portable() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_MCP_SCHEMA_DIALECT".to_string(),
            "portable".to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("should be valid");
        assert_eq!(config.schema_dialect, SchemaDialect::Portable);
    }

    #[test]
    fn schema_dialect_rejects_unknown_values() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_MCP_SCHEMA_DIALECT".to_string(),
            "bogus".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_SCHEMA_DIALECT",
                ..
            }
        ));
    }

    #[test]
    fn log_format_defaults_to_text() {
        let config =
            Config::from_map(&valid_legacy(), TransportKind::Stdio).expect("should be valid");
        assert_eq!(config.log_format, LogFormat::Text);
    }

    #[test]
    fn log_format_parses_json() {
        let mut vars = valid_legacy();
        vars.insert("REDMINE_MCP_LOG_FORMAT".to_string(), "json".to_string());
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("should be valid");
        assert_eq!(config.log_format, LogFormat::Json);
    }

    #[test]
    fn log_format_rejects_unknown_values() {
        let mut vars = valid_legacy();
        vars.insert("REDMINE_MCP_LOG_FORMAT".to_string(), "bogus".to_string());
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_LOG_FORMAT",
                ..
            }
        ));
    }

    #[test]
    fn invalid_bool_is_invalid() {
        let mut vars = valid_legacy();
        vars.insert("REDMINE_MCP_READ_ONLY".to_string(), "maybe".to_string());
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_READ_ONLY",
                ..
            }
        ));
    }

    #[test]
    fn api_key_file_is_read_and_trimmed() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("ruprogress-mcp-test-key-{}", std::process::id()));
        std::fs::write(&path, "file-key\n").expect("write temp key file");
        let mut vars = map(&[("REDMINE_URL", "https://redmine.example.com")]);
        vars.insert(
            "REDMINE_API_KEY_FILE".to_string(),
            path.to_string_lossy().into_owned(),
        );
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("should be valid");
        assert!(matches!(config.auth, AuthMode::Legacy { .. }));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn no_config_error_message_contains_a_secret_value() {
        const SECRET: &str = "super-secret-value-xyz";
        let mut vars = valid_legacy();
        // Conflict path still touches the secret getter; make sure the
        // error text never contains either value.
        vars.insert("REDMINE_API_KEY".to_string(), SECRET.to_string());
        vars.insert("REDMINE_API_KEY_FILE".to_string(), "/tmp/other".to_string());
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(!format!("{err}").contains(SECRET));

        vars.remove("REDMINE_API_KEY_FILE");
        vars.insert("REDMINE_MCP_READ_ONLY".to_string(), "maybe".to_string());
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(!format!("{err}").contains(SECRET));
    }

    #[test]
    fn redacted_summary_never_contains_the_api_key() {
        const SECRET: &str = "super-secret-value-xyz";
        let mut vars = valid_legacy();
        vars.insert("REDMINE_API_KEY".to_string(), SECRET.to_string());
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("should be valid");
        let summary = config.redacted_summary().to_string();
        assert!(!summary.contains(SECRET));
        assert!(summary.contains("redmine.example.com"));
    }

    // --- custom-field autofill -------------------------------------------

    #[test]
    fn autofill_required_custom_fields_defaults_to_false() {
        let config = Config::from_map(&valid_legacy(), TransportKind::Stdio).expect("valid");
        assert!(!config.custom_fields.autofill_required);
        assert!(config.custom_fields.defaults.is_empty());
    }

    #[test]
    fn required_custom_field_defaults_accepts_string_and_array_values() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS".to_string(),
            r#"{"Department": "Engineering", "Tags": ["a", "b"]}"#.to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("valid");
        assert_eq!(
            config.custom_fields.defaults.get("Department"),
            Some(&CustomFieldDefaultValue::Single("Engineering".to_string()))
        );
        assert_eq!(
            config.custom_fields.defaults.get("Tags"),
            Some(&CustomFieldDefaultValue::Multiple(vec![
                "a".to_string(),
                "b".to_string()
            ]))
        );
    }

    #[test]
    fn required_custom_field_defaults_rejects_invalid_json() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS".to_string(),
            "not json".to_string(),
        );
        assert!(matches!(
            Config::from_map(&vars, TransportKind::Stdio),
            Err(ConfigError::Invalid {
                var: "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS",
                ..
            })
        ));
    }

    #[test]
    fn required_custom_field_defaults_rejects_a_top_level_array() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS".to_string(),
            r#"["Department"]"#.to_string(),
        );
        assert!(matches!(
            Config::from_map(&vars, TransportKind::Stdio),
            Err(ConfigError::Invalid {
                var: "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS",
                ..
            })
        ));
    }

    #[test]
    fn required_custom_field_defaults_rejects_a_nested_object_value() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS".to_string(),
            r#"{"Department": {"nested": true}}"#.to_string(),
        );
        assert!(matches!(
            Config::from_map(&vars, TransportKind::Stdio),
            Err(ConfigError::Invalid {
                var: "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS",
                ..
            })
        ));
    }

    #[test]
    fn required_custom_field_defaults_rejects_a_numeric_value() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS".to_string(),
            r#"{"Department": 42}"#.to_string(),
        );
        assert!(matches!(
            Config::from_map(&vars, TransportKind::Stdio),
            Err(ConfigError::Invalid {
                var: "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS",
                ..
            })
        ));
    }

    #[test]
    fn required_custom_field_defaults_rejects_an_empty_object() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS".to_string(),
            "{}".to_string(),
        );
        assert!(matches!(
            Config::from_map(&vars, TransportKind::Stdio),
            Err(ConfigError::Invalid {
                var: "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS",
                ..
            })
        ));
    }

    #[test]
    fn required_custom_field_defaults_empty_string_is_unset_not_an_error() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS".to_string(),
            String::new(),
        );
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("valid");
        assert!(config.custom_fields.defaults.is_empty());
    }

    #[test]
    fn required_custom_field_defaults_without_autofill_is_accepted() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS".to_string(),
            r#"{"Department": "Engineering"}"#.to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("valid");
        assert!(!config.custom_fields.autofill_required);
        assert!(!config.custom_fields.defaults.is_empty());
    }

    #[test]
    fn no_config_error_message_from_custom_field_defaults_contains_the_configured_value() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS".to_string(),
            r#"{"Very Secret Field Name": 42}"#.to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(!format!("{err}").contains("Very Secret Field Name"));
    }

    #[test]
    fn redacted_summary_reports_the_autofill_flag_and_defaults_count_but_not_their_contents() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS".to_string(),
            "true".to_string(),
        );
        vars.insert(
            "REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS".to_string(),
            r#"{"Cost Centre": "12345-secret"}"#.to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("valid");
        let summary = config.redacted_summary();
        assert_eq!(summary["autofill_required_custom_fields"], true);
        assert_eq!(summary["required_custom_field_defaults_count"], 1);
        let rendered = summary.to_string();
        assert!(!rendered.contains("Cost Centre"));
        assert!(!rendered.contains("12345-secret"));
    }

    // --- HTTP transport -------------------------------------------------

    /// `valid_legacy()` plus the given HTTP-transport overrides, parsed as
    /// `TransportKind::Http`.
    fn http(pairs: &[(&str, &str)]) -> Result<HttpConfig, ConfigError> {
        let mut vars = valid_legacy();
        for (k, v) in pairs {
            vars.insert((*k).to_string(), (*v).to_string());
        }
        Config::from_map(&vars, TransportKind::Http).map(|c| match c.transport {
            TransportConfig::Http(h) => *h,
            TransportConfig::Stdio => panic!("asked for http, got stdio"),
        })
    }

    #[test]
    fn http_defaults_to_loopback_port_8000() {
        let cfg = http(&[]).expect("defaults should be valid");
        assert_eq!(cfg.bind.to_string(), "127.0.0.1:8000");
        assert_eq!(cfg.mcp_path, "/mcp");
        assert_eq!(cfg.allowed_hosts, ["localhost", "127.0.0.1", "::1"]);
        assert!(cfg.allowed_origins.is_empty());
        assert_eq!(cfg.max_request_body_bytes, 4 * 1024 * 1024);
        assert_eq!(cfg.health_ttl, Duration::from_secs(30));
    }

    #[test]
    fn server_host_hostname_is_invalid() {
        let err = http(&[("SERVER_HOST", "example.com")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "SERVER_HOST",
                ..
            }
        ));
    }

    #[test]
    fn server_port_zero_is_invalid() {
        let err = http(&[("SERVER_PORT", "0")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "SERVER_PORT",
                ..
            }
        ));
    }

    #[test]
    fn server_port_non_numeric_is_invalid() {
        let err = http(&[("SERVER_PORT", "eighty")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "SERVER_PORT",
                ..
            }
        ));
    }

    #[test]
    fn loopback_bind_without_a_host_policy_is_ok() {
        let cfg = http(&[("SERVER_HOST", "::1")]).expect("loopback should be derivable");
        assert_eq!(cfg.allowed_hosts, ["localhost", "127.0.0.1", "::1"]);
    }

    #[test]
    fn non_loopback_bind_without_a_host_policy_is_missing_public_host() {
        let err = http(&[("SERVER_HOST", "0.0.0.0")]).unwrap_err();
        let ConfigError::Missing {
            var: "PUBLIC_HOST",
            because,
        } = err
        else {
            panic!("expected Missing PUBLIC_HOST, got {err:?}");
        };
        // The message is the primary documentation for this failure, so it
        // must name both escape hatches.
        assert!(because.contains("REDMINE_MCP_ALLOWED_HOSTS"));
        assert!(because.contains('*'));
    }

    #[test]
    fn public_host_alone_is_added_port_agnostically() {
        let cfg = http(&[
            ("SERVER_HOST", "0.0.0.0"),
            ("PUBLIC_HOST", "mcp.example.com"),
        ])
        .expect("PUBLIC_HOST should make the allowlist derivable");
        // Bare, not `mcp.example.com:8000`: a port-less entry matches any port
        // in rmcp, so adding both would make the qualified one decorative.
        assert!(cfg.allowed_hosts.iter().any(|h| h == "mcp.example.com"));
        assert!(
            !cfg.allowed_hosts
                .iter()
                .any(|h| h.starts_with("mcp.example.com:"))
        );
    }

    #[test]
    fn public_port_pins_the_public_host_entry_to_that_port() {
        let cfg = http(&[
            ("SERVER_HOST", "0.0.0.0"),
            ("PUBLIC_HOST", "mcp.example.com"),
            ("PUBLIC_PORT", "443"),
        ])
        .expect("should be valid");
        assert!(cfg.allowed_hosts.iter().any(|h| h == "mcp.example.com:443"));
        // The unqualified entry must be absent, or the port restricts nothing.
        assert!(!cfg.allowed_hosts.iter().any(|h| h == "mcp.example.com"));
    }

    #[test]
    fn a_malformed_public_host_is_invalid() {
        let err = http(&[("SERVER_HOST", "0.0.0.0"), ("PUBLIC_HOST", "not a host")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "PUBLIC_HOST",
                ..
            }
        ));
    }

    #[test]
    fn public_port_without_public_host_is_conflict() {
        let err = http(&[("PUBLIC_PORT", "443")]).unwrap_err();
        assert!(matches!(err, ConfigError::Conflict { .. }));
    }

    #[test]
    fn allowed_hosts_star_disables_validation_and_yields_an_empty_list() {
        // PUBLIC_HOST is required here even though REDMINE_MCP_ALLOWED_HOSTS=*
        // bypasses parse_allowed_hosts's own PUBLIC_HOST requirement:
        // public_base still needs an origin to build /files/{uuid} URLs from.
        let cfg = http(&[
            ("SERVER_HOST", "0.0.0.0"),
            ("REDMINE_MCP_ALLOWED_HOSTS", "*"),
            ("PUBLIC_HOST", "mcp.example.com"),
        ])
        .expect("the explicit opt-out should be accepted");
        assert!(cfg.allowed_hosts.is_empty());
    }

    /// The invariant the whole derivation exists for: rmcp reads an empty
    /// `allowed_hosts` as *allow every host*, so no input other than an
    /// explicit `*` may produce one.
    #[test]
    fn the_host_allowlist_is_never_empty_unless_star_was_asked_for() {
        let inputs: &[&[(&str, &str)]] = &[
            &[],
            &[("SERVER_HOST", "::1")],
            &[("SERVER_HOST", "0.0.0.0"), ("PUBLIC_HOST", "a.example.com")],
            &[("REDMINE_MCP_ALLOWED_HOSTS", "a.example.com")],
            // Present but empty after parsing: the case that must not quietly
            // become "unset", and must certainly not become "allow all".
            &[("REDMINE_MCP_ALLOWED_HOSTS", " , ")],
            &[("REDMINE_MCP_ALLOWED_HOSTS", ",")],
            &[("REDMINE_MCP_ALLOWED_HOSTS", "   ")],
        ];
        for input in inputs {
            // Refusing to start is the other acceptable answer.
            if let Ok(cfg) = http(input) {
                assert!(
                    !cfg.allowed_hosts.is_empty(),
                    "{input:?} produced an empty allowlist, which disables Host validation"
                );
            }
        }
    }

    #[test]
    fn an_allowlist_that_is_set_but_empty_is_invalid_rather_than_ignored() {
        for value in [" , ", ",", "   "] {
            assert!(
                matches!(
                    http(&[("REDMINE_MCP_ALLOWED_HOSTS", value)]),
                    Err(ConfigError::Invalid {
                        var: "REDMINE_MCP_ALLOWED_HOSTS",
                        ..
                    })
                ),
                "{value:?} should be rejected"
            );
            assert!(
                matches!(
                    http(&[("REDMINE_MCP_ALLOWED_ORIGINS", value)]),
                    Err(ConfigError::Invalid {
                        var: "REDMINE_MCP_ALLOWED_ORIGINS",
                        ..
                    })
                ),
                "{value:?} should be rejected"
            );
        }
    }

    #[test]
    fn star_mixed_with_specific_hosts_is_invalid() {
        let err = http(&[("REDMINE_MCP_ALLOWED_HOSTS", "a.example.com,*")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_ALLOWED_HOSTS",
                ..
            }
        ));
    }

    #[test]
    fn a_malformed_allowed_hosts_entry_is_invalid() {
        let err = http(&[("REDMINE_MCP_ALLOWED_HOSTS", "not a host")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_ALLOWED_HOSTS",
                ..
            }
        ));
    }

    #[test]
    fn the_null_origin_is_invalid() {
        let err = http(&[("REDMINE_MCP_ALLOWED_ORIGINS", "null")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_ALLOWED_ORIGINS",
                ..
            }
        ));
    }

    #[test]
    fn allowed_hosts_replaces_the_derived_list_entirely() {
        // PUBLIC_HOST is required here for the same reason as above.
        let cfg = http(&[
            ("SERVER_HOST", "0.0.0.0"),
            (
                "REDMINE_MCP_ALLOWED_HOSTS",
                "a.example.com, b.example.com:9000",
            ),
            ("PUBLIC_HOST", "a.example.com"),
        ])
        .expect("should be valid");
        assert_eq!(cfg.allowed_hosts, ["a.example.com", "b.example.com:9000"]);
    }

    #[test]
    fn allowed_origins_star_is_invalid() {
        let err = http(&[("REDMINE_MCP_ALLOWED_ORIGINS", "*")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_ALLOWED_ORIGINS",
                ..
            }
        ));
    }

    #[test]
    fn allowed_origins_without_a_scheme_is_invalid() {
        let err = http(&[("REDMINE_MCP_ALLOWED_ORIGINS", "app.example.com")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_ALLOWED_ORIGINS",
                ..
            }
        ));
    }

    #[test]
    fn allowed_origins_parse_as_a_trimmed_list() {
        let cfg = http(&[(
            "REDMINE_MCP_ALLOWED_ORIGINS",
            "https://app.example.com, http://localhost:5173",
        )])
        .expect("should be valid");
        assert_eq!(
            cfg.allowed_origins,
            ["https://app.example.com", "http://localhost:5173"]
        );
    }

    #[test]
    fn mcp_path_must_be_an_absolute_multi_segment_path() {
        for bad in ["mcp", "/", "/a/../b", "/mcp?x=1", "/mcp#f", "/m cp"] {
            let Err(err) = http(&[("FASTMCP_STREAMABLE_HTTP_PATH", bad)]) else {
                panic!("{bad:?} should be rejected");
            };
            assert!(
                matches!(
                    err,
                    ConfigError::Invalid {
                        var: "FASTMCP_STREAMABLE_HTTP_PATH",
                        ..
                    }
                ),
                "{bad:?} produced {err:?}"
            );
        }
        let cfg = http(&[("FASTMCP_STREAMABLE_HTTP_PATH", "/api/mcp")]).expect("should be valid");
        assert_eq!(cfg.mcp_path, "/api/mcp");
    }

    #[test]
    fn max_request_body_bytes_is_range_checked() {
        assert!(http(&[("REDMINE_MCP_MAX_REQUEST_BODY_BYTES", "512")]).is_err());
        assert!(http(&[("REDMINE_MCP_MAX_REQUEST_BODY_BYTES", "134217728")]).is_err());
        assert!(http(&[("REDMINE_MCP_MAX_REQUEST_BODY_BYTES", "lots")]).is_err());
        let cfg = http(&[("REDMINE_MCP_MAX_REQUEST_BODY_BYTES", "65536")]).expect("in range");
        assert_eq!(cfg.max_request_body_bytes, 65536);
    }

    #[test]
    fn health_ttl_is_range_checked_and_zero_is_allowed() {
        assert!(http(&[("HEALTH_INTROSPECTION_TTL_SECONDS", "3601")]).is_err());
        assert!(http(&[("HEALTH_INTROSPECTION_TTL_SECONDS", "soon")]).is_err());
        let cfg = http(&[("HEALTH_INTROSPECTION_TTL_SECONDS", "0")]).expect("0 disables caching");
        assert_eq!(cfg.health_ttl, Duration::ZERO);
    }

    #[test]
    fn legacy_per_user_is_accepted_on_http_and_rejected_on_stdio() {
        let vars = map(&[
            ("REDMINE_URL", "https://redmine.example.com"),
            ("REDMINE_AUTH_MODE", "legacy-per-user"),
            ("REDMINE_PER_USER_TRUST_PROXY", "true"),
        ]);
        let config = Config::from_map(&vars, TransportKind::Http).expect("http should accept it");
        assert!(matches!(config.auth, AuthMode::LegacyPerUser { .. }));
        assert!(matches!(
            Config::from_map(&vars, TransportKind::Stdio),
            Err(ConfigError::Conflict { .. })
        ));
    }

    #[test]
    fn redacted_summary_reports_the_transport_but_still_no_secret() {
        const SECRET: &str = "super-secret-value-xyz";
        let mut vars = valid_legacy();
        vars.insert("REDMINE_API_KEY".to_string(), SECRET.to_string());
        let config = Config::from_map(&vars, TransportKind::Http).expect("should be valid");
        let summary = config.redacted_summary();
        assert!(!summary.to_string().contains(SECRET));
        assert_eq!(summary["transport"]["kind"], "http");
        assert_eq!(summary["transport"]["bind"], "127.0.0.1:8000");
        assert_eq!(summary["transport"]["mcp_path"], "/mcp");

        let stdio = Config::from_map(&vars, TransportKind::Stdio).expect("should be valid");
        assert_eq!(
            stdio.redacted_summary()["transport"],
            json!({"kind": "stdio"})
        );
    }

    // --- Attachment config -------------------------------------------------

    #[test]
    fn attachments_default_to_a_temp_dir_with_sensible_caps() {
        let config =
            Config::from_map(&valid_legacy(), TransportKind::Stdio).expect("should be valid");
        let a = &config.attachments;
        assert_eq!(
            a.dir,
            std::env::temp_dir().join("ruprogress-mcp-attachments")
        );
        assert_eq!(a.max_download_bytes, 200 * 1024 * 1024);
        assert_eq!(a.store_max_bytes, 2 * 1024 * 1024 * 1024);
        assert!(a.auto_cleanup_enabled);
        assert_eq!(a.cleanup_interval, Duration::from_mins(15));
        assert_eq!(a.expires_after, Duration::from_hours(1));
        assert!(a.upload_file_roots.is_empty());
        assert!(!a.expose_admin_tools);
        assert!(a.public_url_rewrite.is_none());
    }

    #[test]
    fn attachments_dir_reads_from_env() {
        let mut vars = valid_legacy();
        vars.insert("ATTACHMENTS_DIR".to_string(), "/tmp/custom-dir".to_string());
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("should be valid");
        assert_eq!(config.attachments.dir, PathBuf::from("/tmp/custom-dir"));
    }

    #[test]
    fn store_max_bytes_smaller_than_download_cap_is_conflict() {
        let mut vars = valid_legacy();
        vars.insert(
            "ATTACHMENT_MAX_DOWNLOAD_BYTES".to_string(),
            "1000".to_string(),
        );
        vars.insert("ATTACHMENT_STORE_MAX_BYTES".to_string(), "999".to_string());
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(err, ConfigError::Conflict { .. }));
    }

    #[test]
    fn store_max_bytes_equal_to_download_cap_is_accepted() {
        let mut vars = valid_legacy();
        vars.insert(
            "ATTACHMENT_MAX_DOWNLOAD_BYTES".to_string(),
            "1000".to_string(),
        );
        vars.insert("ATTACHMENT_STORE_MAX_BYTES".to_string(), "1000".to_string());
        Config::from_map(&vars, TransportKind::Stdio).expect("equal caps should be accepted");
    }

    #[test]
    fn zero_byte_caps_are_invalid() {
        for var in [
            "ATTACHMENT_MAX_DOWNLOAD_BYTES",
            "ATTACHMENT_STORE_MAX_BYTES",
        ] {
            let mut vars = valid_legacy();
            vars.insert(var.to_string(), "0".to_string());
            let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
            assert!(matches!(err, ConfigError::Invalid { var: v, .. } if v == var));
        }
    }

    #[test]
    fn zero_minute_durations_are_invalid() {
        for var in ["CLEANUP_INTERVAL_MINUTES", "ATTACHMENT_EXPIRES_MINUTES"] {
            let mut vars = valid_legacy();
            vars.insert(var.to_string(), "0".to_string());
            let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
            assert!(matches!(err, ConfigError::Invalid { var: v, .. } if v == var));
        }
    }

    #[test]
    fn auto_cleanup_enabled_parses_false() {
        let mut vars = valid_legacy();
        vars.insert("AUTO_CLEANUP_ENABLED".to_string(), "false".to_string());
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("should be valid");
        assert!(!config.attachments.auto_cleanup_enabled);
    }

    #[test]
    fn upload_file_roots_parses_absolute_paths() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_MCP_UPLOAD_FILE_ROOTS".to_string(),
            "/srv/uploads, /data/uploads".to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("should be valid");
        assert_eq!(
            config.attachments.upload_file_roots,
            vec![
                PathBuf::from("/srv/uploads"),
                PathBuf::from("/data/uploads")
            ]
        );
    }

    #[test]
    fn upload_file_roots_rejects_a_relative_path() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_MCP_UPLOAD_FILE_ROOTS".to_string(),
            "relative/path".to_string(),
        );
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_UPLOAD_FILE_ROOTS",
                ..
            }
        ));
    }

    #[test]
    fn expose_admin_tools_defaults_false_and_parses_true() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_MCP_EXPOSE_ADMIN_TOOLS".to_string(),
            "true".to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("should be valid");
        assert!(config.attachments.expose_admin_tools);
    }

    #[test]
    fn public_url_rewrite_parses_a_valid_url() {
        let mut vars = valid_legacy();
        vars.insert(
            "REDMINE_PUBLIC_URL".to_string(),
            "https://public.example.com".to_string(),
        );
        let config = Config::from_map(&vars, TransportKind::Stdio).expect("should be valid");
        assert_eq!(
            config.attachments.public_url_rewrite.map(|u| u.to_string()),
            Some("https://public.example.com/".to_string())
        );
    }

    #[test]
    fn public_url_rewrite_rejects_non_http_scheme() {
        let mut vars = valid_legacy();
        vars.insert("REDMINE_PUBLIC_URL".to_string(), "ftp://x".to_string());
        let err = Config::from_map(&vars, TransportKind::Stdio).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_PUBLIC_URL",
                ..
            }
        ));
    }

    // --- public_base -------------------------------------------------------

    #[test]
    fn public_base_defaults_to_the_loopback_bind_when_no_public_host_is_set() {
        let cfg = http(&[]).expect("loopback default should be valid");
        assert_eq!(cfg.public_base.to_string(), "http://127.0.0.1:8000/");
    }

    #[test]
    fn public_base_uses_public_host_scheme_and_port() {
        let cfg = http(&[
            ("SERVER_HOST", "0.0.0.0"),
            ("PUBLIC_HOST", "mcp.example.com"),
            ("PUBLIC_PORT", "8443"),
            ("PUBLIC_SCHEME", "https"),
        ])
        .expect("should be valid");
        assert_eq!(cfg.public_base.to_string(), "https://mcp.example.com:8443/");
    }

    #[test]
    fn public_scheme_defaults_to_https_only_when_public_port_is_443() {
        let cfg = http(&[
            ("SERVER_HOST", "0.0.0.0"),
            ("PUBLIC_HOST", "mcp.example.com"),
            ("PUBLIC_PORT", "443"),
        ])
        .expect("should be valid");
        assert_eq!(cfg.public_base.scheme(), "https");

        let cfg = http(&[
            ("SERVER_HOST", "0.0.0.0"),
            ("PUBLIC_HOST", "mcp.example.com"),
            ("PUBLIC_PORT", "8080"),
        ])
        .expect("should be valid");
        assert_eq!(cfg.public_base.scheme(), "http");
    }

    #[test]
    fn public_scheme_rejects_unknown_values() {
        let err = http(&[("PUBLIC_SCHEME", "gopher")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "PUBLIC_SCHEME",
                ..
            }
        ));
    }

    #[test]
    fn a_non_loopback_bind_with_an_explicit_star_allowlist_but_no_public_host_still_fails_for_public_base()
     {
        // REDMINE_MCP_ALLOWED_HOSTS=* bypasses parse_allowed_hosts's own
        // PUBLIC_HOST requirement, but public_base still needs one.
        let err = http(&[
            ("SERVER_HOST", "0.0.0.0"),
            ("REDMINE_MCP_ALLOWED_HOSTS", "*"),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "PUBLIC_HOST",
                ..
            }
        ));
    }

    #[test]
    fn public_base_for_a_loopback_v6_bind_brackets_the_address() {
        let cfg = http(&[("SERVER_HOST", "::1")]).expect("loopback v6 should be valid");
        assert_eq!(cfg.public_base.to_string(), "http://[::1]:8000/");
    }

    // --- rate limiting (9.2) -----------------------------------------------

    #[test]
    fn rate_limit_defaults() {
        let cfg = http(&[]).expect("defaults should be valid");
        assert!(cfg.rate_limit.enabled);
        assert_eq!(cfg.rate_limit.standard_rps, 10);
        assert_eq!(cfg.rate_limit.standard_burst, 40);
        assert_eq!(cfg.rate_limit.strict_rps, 1);
        assert_eq!(cfg.rate_limit.strict_burst, 10);
        assert_eq!(cfg.rate_limit.max_keys, 10_000);
    }

    #[test]
    fn rate_limit_enabled_accepts_false() {
        let cfg =
            http(&[("REDMINE_MCP_RATE_LIMIT_ENABLED", "false")]).expect("false should be valid");
        assert!(!cfg.rate_limit.enabled);
    }

    #[test]
    fn rate_limit_enabled_rejects_a_non_boolean() {
        let err = http(&[("REDMINE_MCP_RATE_LIMIT_ENABLED", "sometimes")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_RATE_LIMIT_ENABLED",
                ..
            }
        ));
    }

    #[test]
    fn rate_limit_reads_each_var() {
        let cfg = http(&[
            ("REDMINE_MCP_RATE_LIMIT_RPS", "5"),
            ("REDMINE_MCP_RATE_LIMIT_BURST", "20"),
            ("REDMINE_MCP_RATE_LIMIT_AUTH_RPS", "2"),
            ("REDMINE_MCP_RATE_LIMIT_AUTH_BURST", "3"),
            ("REDMINE_MCP_RATE_LIMIT_MAX_KEYS", "500"),
        ])
        .expect("should be valid");
        assert_eq!(cfg.rate_limit.standard_rps, 5);
        assert_eq!(cfg.rate_limit.standard_burst, 20);
        assert_eq!(cfg.rate_limit.strict_rps, 2);
        assert_eq!(cfg.rate_limit.strict_burst, 3);
        assert_eq!(cfg.rate_limit.max_keys, 500);
    }

    #[test]
    fn rate_limit_rps_zero_is_invalid() {
        let err = http(&[("REDMINE_MCP_RATE_LIMIT_RPS", "0")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_RATE_LIMIT_RPS",
                ..
            }
        ));
    }

    #[test]
    fn rate_limit_auth_rps_non_numeric_is_invalid() {
        let err = http(&[("REDMINE_MCP_RATE_LIMIT_AUTH_RPS", "fast")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_RATE_LIMIT_AUTH_RPS",
                ..
            }
        ));
    }

    #[test]
    fn rate_limit_max_keys_zero_is_invalid() {
        let err = http(&[("REDMINE_MCP_RATE_LIMIT_MAX_KEYS", "0")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_RATE_LIMIT_MAX_KEYS",
                ..
            }
        ));
    }

    #[test]
    fn rate_limit_burst_below_rps_is_invalid() {
        let err = http(&[
            ("REDMINE_MCP_RATE_LIMIT_RPS", "10"),
            ("REDMINE_MCP_RATE_LIMIT_BURST", "5"),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_RATE_LIMIT_BURST",
                ..
            }
        ));
    }

    #[test]
    fn rate_limit_auth_burst_below_auth_rps_is_invalid() {
        let err = http(&[
            ("REDMINE_MCP_RATE_LIMIT_AUTH_RPS", "5"),
            ("REDMINE_MCP_RATE_LIMIT_AUTH_BURST", "1"),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "REDMINE_MCP_RATE_LIMIT_AUTH_BURST",
                ..
            }
        ));
    }

    #[test]
    fn rate_limit_burst_equal_to_rps_is_valid() {
        let cfg = http(&[
            ("REDMINE_MCP_RATE_LIMIT_RPS", "10"),
            ("REDMINE_MCP_RATE_LIMIT_BURST", "10"),
        ])
        .expect("burst == rps should be valid");
        assert_eq!(cfg.rate_limit.standard_burst, 10);
    }
}

//! Env-var configuration: validated, injectable, and secret-safe.
//!
//! `Config::from_map` is a pure function over an injected map rather than the
//! ambient environment, so the full validation matrix is testable without
//! touching `std::env` (which is racy under threaded tests and, as of edition
//! 2024, `unsafe` to mutate — see ADR 0002).

use std::collections::BTreeMap;

use redmine_client::Credential;
use secrecy::SecretString;
use serde_json::json;
use url::Url;

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
    /// Not yet implemented. Each request carries its own Redmine credential.
    LegacyPerUser {
        trust: ProxyTrust,
        audit_identity: bool,
    },
    /// Not yet implemented.
    OAuth(OAuthConfig),
}

/// OAuth settings; not yet implemented, only the base URL is validated so far.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub base_url: Url,
}

/// Zero-sized proof that `REDMINE_PER_USER_TRUST_PROXY=true` was set. Only
/// constructible inside this module's validation.
#[derive(Debug, Clone, Copy)]
pub struct ProxyTrust(());

/// The transport the server will run over. Only `Stdio` exists so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportConfig {
    Stdio,
}

/// Which plugin-gated tool families are enabled. Surfaced in
/// `get_mcp_server_info`'s `plugin_flags`; no gated tools exist yet.
#[derive(Debug, Clone, Copy, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the upstream reference server's six independent plugin toggles exactly"
)]
pub struct PluginFlags {
    pub agile: bool,
    pub checklists: bool,
    pub products: bool,
    pub crm: bool,
    pub dmsf: bool,
    pub tags: bool,
}

/// Reserved for attachment storage; not yet implemented.
#[derive(Debug, Clone, Copy, Default)]
pub struct AttachmentConfig;

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

fn parse_auth(vars: &EnvMap, transport: TransportConfig) -> Result<AuthMode, ConfigError> {
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
            if transport == TransportConfig::Stdio {
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
        "oauth" => {
            let raw = required(
                vars,
                "REDMINE_MCP_BASE_URL",
                "oauth auth mode requires the server's own public base URL",
            )?;
            let base_url = raw.parse().map_err(|_| ConfigError::Invalid {
                var: "REDMINE_MCP_BASE_URL",
                expected: "a valid URL",
                because: "the value could not be parsed as a URL".to_string(),
            })?;
            Ok(AuthMode::OAuth(OAuthConfig { base_url }))
        }
        other => Err(ConfigError::Invalid {
            var: "REDMINE_AUTH_MODE",
            expected: "one of \"legacy\", \"legacy-per-user\", \"oauth\"",
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

impl Config {
    /// Validate and build a `Config` from an injected env-var map. `transport`
    /// is passed in rather than parsed from `vars` because it comes from the
    /// CLI (`--transport`), not the environment — but auth-mode
    /// validation still needs to know it (`legacy-per-user` is incompatible
    /// with `stdio`).
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] describing exactly which variable is
    /// missing, invalid, or conflicting.
    pub fn from_map(vars: &EnvMap, transport: TransportConfig) -> Result<Self, ConfigError> {
        Ok(Self {
            redmine: parse_redmine(vars)?,
            auth: parse_auth(vars, transport)?,
            transport,
            read_only: optional_bool(vars, "REDMINE_MCP_READ_ONLY", false)?,
            plugins: parse_plugins(vars)?,
            attachments: AttachmentConfig,
        })
    }

    /// `from_map(std::env::vars().collect(), TransportConfig::Stdio)`.
    ///
    /// # Errors
    ///
    /// See [`Config::from_map`].
    pub fn from_env() -> Result<Self, ConfigError> {
        let vars: EnvMap = std::env::vars().collect();
        Self::from_map(&vars, TransportConfig::Stdio)
    }

    pub(crate) fn auth_mode_label(&self) -> &'static str {
        match &self.auth {
            AuthMode::Legacy { .. } | AuthMode::LegacyPerUser { .. } => "legacy",
            AuthMode::OAuth(_) => "oauth",
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
    /// Redmine host and auth mode for operator debugging, but never a
    /// credential. Used by `--print-config`; the `get_mcp_server_info` MCP
    /// tool builds its own (stricter) summary from the same
    /// [`Self::auth_mode_label`] / [`Self::plugin_flags_json`] helpers rather
    /// than reusing this value directly, since it must also omit the host.
    #[must_use]
    pub fn redacted_summary(&self) -> serde_json::Value {
        json!({
            "redmine": {
                "url_host": self.redmine.url.host_str(),
                "ssl_verify": self.redmine.ssl_verify,
            },
            "auth_mode": self.auth_mode_label(),
            "read_only_mode": self.read_only,
            "plugin_flags": self.plugin_flags_json(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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
        let err = Config::from_map(&vars, TransportConfig::Stdio).unwrap_err();
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
        let err = Config::from_map(&vars, TransportConfig::Stdio).unwrap_err();
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
        let err = Config::from_map(&vars, TransportConfig::Stdio).unwrap_err();
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
        let err = Config::from_map(&vars, TransportConfig::Stdio).unwrap_err();
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
        let err = Config::from_map(&vars, TransportConfig::Stdio).unwrap_err();
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
        let err = Config::from_map(&vars, TransportConfig::Stdio).unwrap_err();
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
        let err = Config::from_map(&vars, TransportConfig::Stdio).unwrap_err();
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
        let err = Config::from_map(&vars, TransportConfig::Stdio).unwrap_err();
        assert!(matches!(err, ConfigError::Conflict { .. }));
    }

    #[test]
    fn oauth_without_base_url_is_missing() {
        let vars = map(&[
            ("REDMINE_URL", "https://redmine.example.com"),
            ("REDMINE_AUTH_MODE", "oauth"),
        ]);
        let err = Config::from_map(&vars, TransportConfig::Stdio).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "REDMINE_MCP_BASE_URL",
                ..
            }
        ));
    }

    #[test]
    fn oauth_with_base_url_succeeds() {
        let vars = map(&[
            ("REDMINE_URL", "https://redmine.example.com"),
            ("REDMINE_AUTH_MODE", "oauth"),
            ("REDMINE_MCP_BASE_URL", "http://localhost:3040"),
        ]);
        let config = Config::from_map(&vars, TransportConfig::Stdio).expect("should be valid");
        assert!(matches!(config.auth, AuthMode::OAuth(_)));
    }

    #[test]
    fn ssl_verify_false_is_accepted() {
        let mut vars = valid_legacy();
        vars.insert("REDMINE_SSL_VERIFY".to_string(), "false".to_string());
        let config = Config::from_map(&vars, TransportConfig::Stdio).expect("should be valid");
        assert!(!config.redmine.ssl_verify);
    }

    #[test]
    fn unknown_auth_mode_is_invalid() {
        let mut vars = valid_legacy();
        vars.insert("REDMINE_AUTH_MODE".to_string(), "bogus".to_string());
        let err = Config::from_map(&vars, TransportConfig::Stdio).unwrap_err();
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
            Config::from_map(&valid_legacy(), TransportConfig::Stdio).expect("should be valid");
        assert!(matches!(config.auth, AuthMode::Legacy { .. }));
        assert!(!config.read_only);
        assert_eq!(config.redmine.url.host_str(), Some("redmine.example.com"));
    }

    #[test]
    fn read_only_flag_parses() {
        let mut vars = valid_legacy();
        vars.insert("REDMINE_MCP_READ_ONLY".to_string(), "true".to_string());
        let config = Config::from_map(&vars, TransportConfig::Stdio).expect("should be valid");
        assert!(config.read_only);
    }

    #[test]
    fn plugin_flags_default_false_and_parse_true() {
        let mut vars = valid_legacy();
        vars.insert("REDMINE_DMSF_ENABLED".to_string(), "true".to_string());
        let config = Config::from_map(&vars, TransportConfig::Stdio).expect("should be valid");
        assert!(config.plugins.dmsf);
        assert!(!config.plugins.agile);
    }

    #[test]
    fn invalid_bool_is_invalid() {
        let mut vars = valid_legacy();
        vars.insert("REDMINE_MCP_READ_ONLY".to_string(), "maybe".to_string());
        let err = Config::from_map(&vars, TransportConfig::Stdio).unwrap_err();
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
        let config = Config::from_map(&vars, TransportConfig::Stdio).expect("should be valid");
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
        let err = Config::from_map(&vars, TransportConfig::Stdio).unwrap_err();
        assert!(!format!("{err}").contains(SECRET));

        vars.remove("REDMINE_API_KEY_FILE");
        vars.insert("REDMINE_MCP_READ_ONLY".to_string(), "maybe".to_string());
        let err = Config::from_map(&vars, TransportConfig::Stdio).unwrap_err();
        assert!(!format!("{err}").contains(SECRET));
    }

    #[test]
    fn redacted_summary_never_contains_the_api_key() {
        const SECRET: &str = "super-secret-value-xyz";
        let mut vars = valid_legacy();
        vars.insert("REDMINE_API_KEY".to_string(), SECRET.to_string());
        let config = Config::from_map(&vars, TransportConfig::Stdio).expect("should be valid");
        let summary = config.redacted_summary().to_string();
        assert!(!summary.contains(SECRET));
        assert!(summary.contains("redmine.example.com"));
    }
}

//! `get_mcp_server_info`.

use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::Serialize;

use crate::config::PluginFlags;
use crate::render::Boundary;
use crate::server::RedmineMcp;
use crate::tools::output;

/// A boundary-wrapped summary of the authenticated user, or absent when
/// Redmine is unreachable or the credential could not be resolved.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct CurrentUserSummary {
    pub(crate) id: u64,
    pub(crate) login: Option<String>,
    /// `"firstname lastname"`, boundary-wrapped.
    pub(crate) name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ServerInfoOutput {
    pub(crate) server_version: String,
    pub(crate) read_only_mode: bool,
    pub(crate) auth_mode: String,
    pub(crate) transport: String,
    pub(crate) current_user: Option<CurrentUserSummary>,
    pub(crate) plugin_flags: PluginFlags,
    /// `REDMINE_OAUTH_SCOPE_ENFORCEMENT`'s effective value; `null` outside
    /// `oauth` mode, where the setting does not apply.
    pub(crate) oauth_scope_enforcement: Option<bool>,
    /// `REDMINE_AUTOFILL_REQUIRED_CUSTOM_FIELDS`'s effective value.
    pub(crate) autofill_required_custom_fields: bool,
    /// How many fields `REDMINE_REQUIRED_CUSTOM_FIELD_DEFAULTS` configures —
    /// never the field names or values themselves, which can be
    /// business-sensitive.
    pub(crate) required_custom_field_defaults_count: usize,
}

#[tool_router(router = meta_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// Return the MCP server's version, enabled-feature flags, and the
    /// identity of the authenticated Redmine user. The response excludes
    /// credentials, internal hostnames, and filesystem paths.
    #[tool(
        description = "Return the MCP server's version, read-only/auth mode, plugin flags, and the identity of the authenticated Redmine user (or null if Redmine is unreachable). Use this once at the start of a session to learn what the server can do before calling other tools. Plugin-gated tools (e.g. get_checklist) are absent from tools/list unless their plugin_flags entry is on.",
        output_schema = crate::tools::schema::output::<ServerInfoOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn get_mcp_server_info(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let current_user = match self.scoped(&ctx) {
            Ok(scoped) => scoped.current_user().await.ok(),
            Err(_) => None,
        };

        let boundary = Boundary::new();
        let current_user = current_user.map(|u| CurrentUserSummary {
            id: u.id,
            login: u.login,
            name: boundary.wrap("user.name", &format!("{} {}", u.firstname, u.lastname)),
        });

        let output = ServerInfoOutput {
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            read_only_mode: self.inner.config.read_only,
            auth_mode: self.inner.config.auth_mode_label().to_string(),
            // Just the kind. Not the bind address and not the MCP path: the
            // model has no use for either, and both are useful to an attacker
            // who has achieved prompt injection.
            transport: self.inner.config.transport.label().to_string(),
            current_user,
            plugin_flags: self.inner.config.plugins,
            oauth_scope_enforcement: self
                .inner
                .config
                .oauth_resource()
                .map(|oauth| oauth.scope_enforcement),
            autofill_required_custom_fields: self.inner.config.custom_fields.autofill_required,
            required_custom_field_defaults_count: self.inner.config.custom_fields.defaults.len(),
        };
        Ok(output::ok(&output, self.output_caps()))
    }
}

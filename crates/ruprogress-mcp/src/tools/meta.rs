//! `get_mcp_server_info`.

use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use serde_json::json;

use crate::render::Boundary;
use crate::server::RedmineMcp;

#[tool_router(router = meta_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// Return the MCP server's version, enabled-feature flags, and the
    /// identity of the authenticated Redmine user. The response excludes
    /// credentials, internal hostnames, and filesystem paths.
    #[tool(
        description = "Return the MCP server's version, read-only/auth mode, plugin flags, and the identity of the authenticated Redmine user (or null if Redmine is unreachable)."
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
        let (current_user_json, wrapped_any) = match current_user {
            Some(u) => (
                Some(json!({
                    "id": u.id,
                    "login": u.login,
                    "name": boundary.wrap("user.name", &format!("{} {}", u.firstname, u.lastname)),
                })),
                true,
            ),
            None => (None, false),
        };

        let body = json!({
            "server_version": env!("CARGO_PKG_VERSION"),
            "read_only_mode": self.inner.config.read_only,
            "auth_mode": self.inner.config.auth_mode_label(),
            "current_user": current_user_json,
            "plugin_flags": self.inner.config.plugin_flags_json(),
        });

        let mut blocks = Vec::new();
        if wrapped_any {
            blocks.push(ContentBlock::text(boundary.preamble()));
        }
        blocks.push(ContentBlock::text(body.to_string()));
        Ok(CallToolResult::success(blocks))
    }
}

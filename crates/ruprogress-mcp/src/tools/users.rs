//! `get_current_user`.

use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use serde_json::json;

use crate::error::to_mcp_error;
use crate::render::Boundary;
use crate::server::RedmineMcp;

#[tool_router(router = users_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// Resolves to `GET /my/account.json`; works for any authenticated user,
    /// not just admins.
    #[tool(
        description = "Retrieve the currently authenticated user's profile (id, login, name, mail)."
    )]
    pub(crate) async fn get_current_user(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let user = scoped.current_user().await.map_err(to_mcp_error)?;

        let boundary = Boundary::new();
        let body = json!({
            "id": user.id,
            "login": user.login,
            "firstname": boundary.wrap("user.firstname", &user.firstname),
            "lastname": boundary.wrap("user.lastname", &user.lastname),
            "mail": user.mail,
            "created_on": user.created_on,
            "last_login_on": user.last_login_on,
        });

        Ok(CallToolResult::success(vec![
            ContentBlock::text(boundary.preamble()),
            ContentBlock::text(body.to_string()),
        ]))
    }
}

//! `get_current_user`.

use chrono::{DateTime, Utc};
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::Serialize;

use crate::error::to_tool_error;
use crate::render::Boundary;
use crate::server::RedmineMcp;
use crate::tools::output;

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct CurrentUserOutput {
    pub(crate) id: u64,
    pub(crate) login: Option<String>,
    pub(crate) firstname: String,
    pub(crate) lastname: String,
    pub(crate) mail: Option<String>,
    pub(crate) created_on: DateTime<Utc>,
    pub(crate) last_login_on: Option<DateTime<Utc>>,
}

#[tool_router(router = users_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// Resolves to `GET /my/account.json`; works for any authenticated user,
    /// not just admins.
    #[tool(
        description = "Retrieve the currently authenticated user's profile (id, login, name, mail). Use this to resolve \"me\" or to check whether the credential is an admin before calling admin-only tools.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<CurrentUserOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn get_current_user(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let user = match scoped.current_user().await {
            Ok(user) => user,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let output = CurrentUserOutput {
            id: user.id,
            login: user.login,
            firstname: boundary.wrap("user.firstname", &user.firstname),
            lastname: boundary.wrap("user.lastname", &user.lastname),
            mail: user.mail,
            created_on: user.created_on,
            last_login_on: user.last_login_on,
        };
        Ok(output::ok(&output, self.output_caps()))
    }
}

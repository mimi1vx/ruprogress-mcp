//! `list_redmine_projects`.

use redmine_client::model::project::ProjectQuery;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use serde_json::{Value, json};

use crate::error::to_mcp_error;
use crate::render::Boundary;
use crate::server::RedmineMcp;

#[tool_router(router = projects_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// `GET /projects.json`, auto-paged. Takes no parameters — do not add
    /// `limit`/`offset` here; the reference contract has none.
    #[tool(description = "List all accessible projects in the Redmine instance.")]
    pub(crate) async fn list_redmine_projects(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let page = scoped
            .list_projects(&ProjectQuery::default())
            .await
            .map_err(to_mcp_error)?;

        let boundary = Boundary::new();
        let projects: Vec<Value> = page
            .items
            .iter()
            .map(|p| {
                json!({
                    "id": p.id,
                    "name": boundary.wrap("project.name", &p.name),
                    "identifier": p.identifier,
                    "description": p
                        .description
                        .as_deref()
                        .map(|d| boundary.wrap("project.description", d)),
                })
            })
            .collect();

        let mut blocks = Vec::new();
        if !projects.is_empty() {
            blocks.push(ContentBlock::text(boundary.preamble()));
        }
        blocks.push(ContentBlock::text(Value::Array(projects).to_string()));
        Ok(CallToolResult::success(blocks))
    }
}

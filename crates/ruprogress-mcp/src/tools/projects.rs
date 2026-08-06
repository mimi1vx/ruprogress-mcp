//! `list_redmine_projects`.

use redmine_client::model::project::ProjectQuery;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::Serialize;

use crate::error::to_tool_error;
use crate::render::Boundary;
use crate::server::RedmineMcp;
use crate::tools::output::{self, Pagination};

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ProjectOut {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) identifier: String,
    pub(crate) description: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ProjectsOutput {
    pub(crate) projects: Vec<ProjectOut>,
    pub(crate) pagination: Pagination,
}

#[tool_router(router = projects_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// `GET /projects.json`, auto-paged. Takes no parameters — do not add
    /// `limit`/`offset` here; the reference contract has none.
    #[tool(
        description = "List all accessible projects in the Redmine instance. Use this first to resolve a project's numeric id or identifier before calling project- or issue-scoped tools. An empty list means the credential cannot see any projects — do not retry with the same arguments.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<ProjectsOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_redmine_projects(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let page = match scoped.list_projects(&ProjectQuery::default()).await {
            Ok(page) => page,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let pagination = Pagination::from_page(&page);
        let projects = page
            .items
            .iter()
            .map(|p| ProjectOut {
                id: p.id,
                name: boundary.wrap("project.name", &p.name),
                identifier: p.identifier.clone(),
                description: p
                    .description
                    .as_deref()
                    .map(|d| boundary.wrap("project.description", d)),
            })
            .collect();

        Ok(output::ok(
            &ProjectsOutput {
                projects,
                pagination,
            },
            self.output_caps(),
        ))
    }
}

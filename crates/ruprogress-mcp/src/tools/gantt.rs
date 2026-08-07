//! Gantt tool: `get_gantt_chart`.
//!
//! Pure projection over existing `redmine-client` primitives (`get_project`,
//! `list_issues_page`, `list_versions`) — no new client work, per
//! `plans/phase-4f-gantt.md`. Dependency edges are drawn from `Issue.parent`
//! (already present on the default `GET /issues.json` response) rather than
//! a per-issue relations fetch, which would be an uncapped N+1 over up to
//! 500 issues (decision J1).

use chrono::NaiveDate;
use redmine_client::model::issue::{IssueQuery, StatusFilter};
use redmine_client::model::version::Version;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::to_tool_error;
use crate::render::Boundary;
use crate::server::RedmineMcp;
use crate::tools::discovery::{ProjectRef, resolve_project_ref};
use crate::tools::output::{self, Pagination};

const GANTT_MIN_LIMIT: u32 = 1;
const GANTT_MAX_LIMIT: u32 = 500;
const GANTT_DEFAULT_LIMIT: u32 = 100;

/// Clamp to \[1, 500\] (J5/E4): a value outside the range is silently
/// corrected rather than rejected; the effective value is echoed back in
/// `pagination.limit`.
fn clamp_gantt_limit(limit: Option<u32>) -> u32 {
    limit
        .unwrap_or(GANTT_DEFAULT_LIMIT)
        .clamp(GANTT_MIN_LIMIT, GANTT_MAX_LIMIT)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetGanttChartParams {
    /// The project to chart: numeric id or slug identifier.
    pub(crate) project_id: ProjectRef,
    /// Only include issues with `start_date >= this` (`YYYY-MM-DD`).
    #[serde(default)]
    pub(crate) start_date_after: Option<NaiveDate>,
    /// Only include issues with `due_date <= this` (`YYYY-MM-DD`).
    #[serde(default)]
    pub(crate) due_date_before: Option<NaiveDate>,
    /// Include closed issues. Defaults to `false` to keep response size and
    /// pagination cost low on long-lived projects; set to `true` for a full
    /// historical timeline.
    #[serde(default)]
    pub(crate) include_closed: Option<bool>,
    /// Max issues to return, clamped to 1-500. Defaults to 100.
    #[serde(default)]
    pub(crate) limit: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct GanttIssueOut {
    pub(crate) id: u64,
    pub(crate) subject: String,
    /// Not boundary-wrapped: tracker names are instance configuration, same
    /// treatment as elsewhere in this codebase (4a).
    pub(crate) tracker: String,
    pub(crate) status: String,
    pub(crate) start_date: Option<NaiveDate>,
    pub(crate) due_date: Option<NaiveDate>,
    pub(crate) done_ratio: Option<u8>,
    /// The parent issue's id, if this is a sub-issue — the hierarchy edge
    /// to draw on the chart (J1: no separate relations fetch).
    pub(crate) parent_id: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct GanttMilestoneOut {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) due_date: Option<NaiveDate>,
    pub(crate) status: String,
}

fn milestone_out(boundary: &Boundary, v: &Version) -> GanttMilestoneOut {
    GanttMilestoneOut {
        id: v.id,
        name: boundary.wrap("version.name", &v.name),
        due_date: v.due_date,
        status: v.status.clone(),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct GanttChartOutput {
    pub(crate) project_id: u64,
    pub(crate) project_name: String,
    pub(crate) issues: Vec<GanttIssueOut>,
    /// The project's versions (roadmap milestones), unrelated to `pagination`
    /// below — that field describes `issues` only.
    pub(crate) milestones: Vec<GanttMilestoneOut>,
    pub(crate) pagination: Pagination,
}

#[tool_router(router = gantt_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// `GET /issues.json` (date-filtered, one bounded page, J6) plus
    /// `GET /projects/{id}/versions.json` for milestones (J2).
    #[tool(
        description = "Build a Gantt-chart projection for a project: issues with start/due dates, percent-done, and parent hierarchy, plus the project's versions as milestones. Use this when the user wants a timeline or roadmap view rather than a flat issue list. Defaults to open issues only; set include_closed=true for a full historical timeline.",
        input_schema = crate::tools::schema::input::<GetGanttChartParams>(),
        output_schema = crate::tools::schema::output::<GanttChartOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn get_gantt_chart(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<GetGanttChartParams>,
    ) -> Result<CallToolResult, McpError> {
        let project_ident = resolve_project_ref(params.project_id)?;
        let limit = clamp_gantt_limit(params.limit);
        let scoped = self.scoped(&ctx)?;

        let mut query = IssueQuery {
            project: Some(project_ident.clone()),
            status: Some(if params.include_closed.unwrap_or(false) {
                StatusFilter::All
            } else {
                StatusFilter::Open
            }),
            sort: Some("start_date:asc".to_string()),
            ..IssueQuery::default()
        };
        if let Some(after) = params.start_date_after {
            query
                .extra
                .insert("start_date".to_string(), format!(">={after}"));
        }
        if let Some(before) = params.due_date_before {
            query
                .extra
                .insert("due_date".to_string(), format!("<={before}"));
        }

        // Concurrent, not spawned: `Scoped<'a>` borrows the credential, same
        // reasoning as `summarize_project_status` (F2) — `try_join!` gives a
        // fixed, three-request fan-out that never grows with project size.
        let result = tokio::try_join!(
            scoped.get_project(&project_ident, &[]),
            scoped.list_issues_page(&query, limit, 0),
            scoped.list_versions(&project_ident),
        );
        let (project, page, versions) = match result {
            Ok(t) => t,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let pagination = Pagination::from_page(&page);
        let issues = page
            .items
            .iter()
            .map(|i| GanttIssueOut {
                id: i.id,
                subject: boundary.wrap("issue.subject", &i.subject),
                tracker: i.tracker.name.clone(),
                status: i.status.name.clone(),
                start_date: i.start_date,
                due_date: i.due_date,
                done_ratio: i.done_ratio,
                parent_id: i.parent.as_ref().map(|p| p.id),
            })
            .collect();
        let milestones = versions
            .iter()
            .map(|v| milestone_out(&boundary, v))
            .collect();

        Ok(output::ok(
            &GanttChartOutput {
                project_id: project.id,
                project_name: boundary.wrap("project.name", &project.name),
                issues,
                milestones,
                pagination,
            },
            self.output_caps(),
        ))
    }
}

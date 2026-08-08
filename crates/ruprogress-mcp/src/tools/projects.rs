//! Project-management tools: `list_redmine_projects`,
//! `list_project_issue_custom_fields`, `summarize_project_status`,
//! `list_redmine_versions`, `manage_redmine_version`, `list_project_members`,
//! `list_redmine_roles`, `get_project_modules`, `manage_project_member`.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use redmine_client::model::issue::{IssueQuery, StatusFilter};
use redmine_client::model::membership::{Membership, MembershipCreate, MembershipUpdate};
use redmine_client::model::project::{ProjectInclude, ProjectQuery};
use redmine_client::model::version::{SharingMode, Version, VersionStatus, VersionWrite};
use redmine_client::{MembershipId, ProjectId, ProjectIdent, VersionId};
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
use crate::tools::output::{self, ErrorCode, Pagination};

// --- list_redmine_projects ---

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

// --- list_project_issue_custom_fields ---

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListProjectIssueCustomFieldsParams {
    /// The project to list issue custom fields for: numeric id or slug
    /// identifier.
    pub(crate) project_id: ProjectRef,
    /// Restrict output to fields applicable to this tracker id.
    #[serde(default)]
    pub(crate) tracker_id: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct CustomFieldOut {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) field_format: String,
    /// The custom field *definition*'s own required flag. **Not**
    /// authoritative: workflow rules and per-tracker settings can still make
    /// a field effectively required without it being reflected here. A
    /// `create_redmine_issue`/`update_redmine_issue` call can still be
    /// rejected for a field this lists as `is_required: false`.
    pub(crate) is_required: Option<bool>,
    pub(crate) multiple: Option<bool>,
    pub(crate) default_value: Option<String>,
    pub(crate) possible_values: Option<Vec<String>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct CustomFieldsOutput {
    pub(crate) custom_fields: Vec<CustomFieldOut>,
}

// --- summarize_project_status ---

/// Sample size for the status/priority/assignee breakdown — a single
/// bounded `GET /issues.json?limit=100` call, never proportional to project
/// size.
const SUMMARY_SAMPLE_LIMIT: u32 = 100;
const DEFAULT_SUMMARY_DAYS: u32 = 30;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SummarizeProjectStatusParams {
    /// The project to summarize.
    pub(crate) project_id: u64,
    /// Number of days of history to analyze for the recent-activity
    /// counts. Defaults to 30.
    #[serde(default)]
    pub(crate) days: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct RecentActivity {
    pub(crate) created_count: u64,
    pub(crate) updated_count: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ProjectTotals {
    pub(crate) total: u64,
    pub(crate) open: u64,
    pub(crate) closed: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct BreakdownEntry {
    pub(crate) name: String,
    pub(crate) count: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ProjectStatusOutput {
    pub(crate) project_id: u64,
    pub(crate) project_name: String,
    pub(crate) analysis_period_days: u32,
    pub(crate) recent_activity: RecentActivity,
    pub(crate) totals: ProjectTotals,
    pub(crate) status_breakdown: Vec<BreakdownEntry>,
    pub(crate) priority_breakdown: Vec<BreakdownEntry>,
    pub(crate) assignee_breakdown: Vec<BreakdownEntry>,
    /// How many issues the breakdowns above were computed over.
    pub(crate) sample_size: u64,
    /// `true` when `sample_size < totals.total`: the breakdowns are over a
    /// capped sample, not every issue in the project.
    pub(crate) sample_truncated: bool,
}

/// Count occurrences of each `name` in `items`, descending by count then
/// ascending by name for a stable order.
fn bucket_counts(names: impl Iterator<Item = String>) -> Vec<(String, u64)> {
    let mut counts: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for name in names {
        let counter = counts.entry(name).or_insert(0);
        *counter = counter.saturating_add(1);
    }
    let mut pairs: Vec<(String, u64)> = counts.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs
}

// --- list_redmine_versions ---

/// `status`/`status_filter` values accepted at the MCP boundary. A closed
/// enum (rather than a raw `String`) so the generated `inputSchema` carries
/// a JSON Schema `enum` constraint.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum VersionStatusParam {
    Open,
    Locked,
    Closed,
}

impl VersionStatusParam {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Locked => "locked",
            Self::Closed => "closed",
        }
    }
}

impl From<VersionStatusParam> for VersionStatus {
    fn from(p: VersionStatusParam) -> Self {
        match p {
            VersionStatusParam::Open => Self::Open,
            VersionStatusParam::Locked => Self::Locked,
            VersionStatusParam::Closed => Self::Closed,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SharingParam {
    None,
    Descendants,
    Hierarchy,
    Tree,
    System,
}

impl From<SharingParam> for SharingMode {
    fn from(p: SharingParam) -> Self {
        match p {
            SharingParam::None => Self::None,
            SharingParam::Descendants => Self::Descendants,
            SharingParam::Hierarchy => Self::Hierarchy,
            SharingParam::Tree => Self::Tree,
            SharingParam::System => Self::System,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListRedmineVersionsParams {
    /// The project to list versions for: numeric id or slug identifier.
    pub(crate) project_id: ProjectRef,
    /// Filter by version status. Applied after fetching every version —
    /// Redmine's endpoint has no server-side status filter.
    #[serde(default)]
    pub(crate) status_filter: Option<VersionStatusParam>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct VersionOut {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) status: String,
    pub(crate) due_date: Option<NaiveDate>,
    pub(crate) sharing: Option<String>,
    pub(crate) project_id: u64,
}

fn version_out(boundary: &Boundary, v: &Version) -> VersionOut {
    VersionOut {
        id: v.id,
        name: boundary.wrap("version.name", &v.name),
        description: v
            .description
            .as_deref()
            .map(|d| boundary.wrap("version.description", d)),
        status: v.status.clone(),
        due_date: v.due_date,
        sharing: v.sharing.clone(),
        project_id: v.project.id,
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct VersionsOutput {
    pub(crate) versions: Vec<VersionOut>,
}

// --- manage_redmine_version ---

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ManageVersionAction {
    Create,
    Update,
    Delete,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManageRedmineVersionParams {
    /// Operation to perform.
    pub(crate) action: ManageVersionAction,
    /// Project id or identifier. Required for `action = "create"`.
    #[serde(default)]
    pub(crate) project_id: Option<ProjectRef>,
    /// Version id. Required for `action = "update"` and `action = "delete"`.
    #[serde(default)]
    pub(crate) version_id: Option<u64>,
    /// Version name. Required for `action = "create"`, optional for
    /// `action = "update"`.
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    /// Defaults to `open` on create if omitted.
    #[serde(default)]
    pub(crate) status: Option<VersionStatusParam>,
    /// `YYYY-MM-DD`.
    #[serde(default)]
    pub(crate) due_date: Option<NaiveDate>,
    /// Defaults to `none` on create if omitted.
    #[serde(default)]
    pub(crate) sharing: Option<SharingParam>,
    #[serde(default)]
    pub(crate) wiki_page_title: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ManageVersionOutput {
    pub(crate) success: bool,
    /// Populated for `action = "create"`/`"update"`.
    pub(crate) version: Option<VersionOut>,
    /// Populated for `action = "delete"`.
    pub(crate) deleted_version_id: Option<u64>,
}

// --- list_project_members ---

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectIdParams {
    /// Numeric id or slug identifier.
    pub(crate) project_id: ProjectRef,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct PrincipalOut {
    pub(crate) id: u64,
    pub(crate) name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct MembershipOut {
    pub(crate) id: u64,
    pub(crate) user: Option<PrincipalOut>,
    pub(crate) group: Option<PrincipalOut>,
    /// Role names are instance configuration, not user content — never
    /// boundary-wrapped (same treatment as tracker/status names).
    pub(crate) roles: Vec<PrincipalOut>,
}

fn membership_out(boundary: &Boundary, m: &Membership) -> MembershipOut {
    MembershipOut {
        id: m.id,
        user: m.user.as_ref().map(|u| PrincipalOut {
            id: u.id,
            name: boundary.wrap("membership.user.name", &u.name),
        }),
        group: m.group.as_ref().map(|g| PrincipalOut {
            id: g.id,
            name: boundary.wrap("membership.group.name", &g.name),
        }),
        roles: m
            .roles
            .iter()
            .map(|r| PrincipalOut {
                id: r.id,
                name: r.name.clone(),
            })
            .collect(),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct MembershipsOutput {
    pub(crate) memberships: Vec<MembershipOut>,
    pub(crate) pagination: Pagination,
}

// --- list_redmine_roles ---

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct RoleOut {
    pub(crate) id: u64,
    pub(crate) name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct RolesOutput {
    pub(crate) roles: Vec<RoleOut>,
}

// --- get_project_modules ---

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ProjectModulesOutput {
    pub(crate) project_id: u64,
    pub(crate) project_name: String,
    /// Module identifiers (`"issue_tracking"`, `"wiki"`, ...), not
    /// user-controlled content — never boundary-wrapped.
    pub(crate) enabled_modules: Vec<String>,
}

// --- manage_project_member ---

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ManageMemberAction {
    Add,
    Update,
    Remove,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManageProjectMemberParams {
    pub(crate) action: ManageMemberAction,
    /// Required for `action = "add"`.
    #[serde(default)]
    pub(crate) project_id: Option<ProjectRef>,
    /// Required for `action = "update"` and `action = "remove"`.
    #[serde(default)]
    pub(crate) membership_id: Option<u64>,
    /// Exactly one of `user_id`/`group_id` is required for `action = "add"`.
    #[serde(default)]
    pub(crate) user_id: Option<u64>,
    /// Exactly one of `user_id`/`group_id` is required for `action = "add"`.
    #[serde(default)]
    pub(crate) group_id: Option<u64>,
    /// Non-empty. Required for `action = "add"` and `action = "update"`. Use
    /// `list_redmine_roles` to discover valid ids.
    #[serde(default)]
    pub(crate) role_ids: Option<Vec<u64>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ManageMemberOutput {
    pub(crate) success: bool,
    /// Populated for `action = "add"`/`"update"`.
    pub(crate) membership: Option<MembershipOut>,
    /// Populated for `action = "remove"`.
    pub(crate) deleted_membership_id: Option<u64>,
}

#[tool_router(router = projects_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// `GET /projects.json`, auto-paged. Takes no parameters — do not add
    /// `limit`/`offset` here; the reference contract has none.
    #[tool(
        description = "List all accessible projects in the Redmine instance. Use this first to resolve a project's numeric id or identifier before calling project- or issue-scoped tools. An empty list means the credential cannot see any projects — do not retry with the same arguments.",
        output_schema = crate::tools::schema::output::<ProjectsOutput>(),
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

    /// `GET /custom_fields.json` (global, admin-only), filtered client-side
    /// to `customized_type == "issue"` and this project/tracker.
    #[tool(
        description = "List issue custom fields configured for a project, including allowed values and tracker bindings. Use this before create_redmine_issue/update_redmine_issue to discover which fields a project accepts. Requires an admin credential (this endpoint is admin-only regardless of field sensitivity). is_required is not authoritative: workflow rules can still require a field reported as optional.",
        input_schema = crate::tools::schema::input::<ListProjectIssueCustomFieldsParams>(),
        output_schema = crate::tools::schema::output::<CustomFieldsOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_project_issue_custom_fields(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ListProjectIssueCustomFieldsParams>,
    ) -> Result<CallToolResult, McpError> {
        let project_ident = resolve_project_ref(params.project_id)?;
        let scoped = self.scoped(&ctx)?;

        // Resolve to a numeric id even when the caller already passed one:
        // `custom_fields.json`'s `projects` array is keyed by
        // numeric id, and this also gives a `NOT_FOUND` for a bad project
        // instead of a silently empty result.
        let project = match scoped.get_project(&project_ident, &[]).await {
            Ok(project) => project,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let fields = match scoped.list_custom_field_definitions().await {
            Ok(fields) => fields,
            Err(redmine_client::Error::Forbidden) => {
                return Ok(output::err(
                    ErrorCode::Forbidden,
                    "the configured Redmine credential is not an administrator (403 forbidden); GET /custom_fields.json is an admin-only endpoint on Redmine's side",
                    Some(
                        "do not retry; ask the user for an account with admin privileges — the field data itself is not necessarily sensitive, only the endpoint is admin-gated",
                    ),
                ));
            }
            Err(e) => return Ok(to_tool_error(e)),
        };

        let custom_fields = fields
            .into_iter()
            .filter(|f| f.customized_type.as_deref() == Some("issue"))
            .filter(|f| {
                f.is_for_all.unwrap_or(false)
                    || f.projects
                        .as_ref()
                        .is_some_and(|ps| ps.iter().any(|p| p.id == project.id))
            })
            .filter(|f| {
                params.tracker_id.is_none_or(|tracker_id| {
                    f.trackers
                        .as_ref()
                        .is_some_and(|ts| ts.iter().any(|t| t.id == tracker_id))
                })
            })
            .map(|f| CustomFieldOut {
                id: f.id,
                name: f.name,
                field_format: f.field_format,
                is_required: f.is_required,
                multiple: f.multiple,
                default_value: f.default_value,
                possible_values: f.possible_values,
            })
            .collect();

        Ok(output::ok(
            &CustomFieldsOutput { custom_fields },
            self.output_caps(),
        ))
    }

    /// Six fixed HTTP requests, never proportional to project size:
    /// `get_project` for the name, one bounded issue sample
    /// for the status/priority/assignee breakdown, and four `limit=1`
    /// count-only queries (open, closed, created-in-period,
    /// updated-in-period).
    #[tool(
        description = "Summarize project status: recent issue activity, status/priority/assignee breakdowns, and open/closed totals over a configurable time window. Use this when the user wants a written project health summary, not a raw issue list. The breakdowns are computed over a capped recent-issue sample (see sample_truncated), not necessarily every issue.",
        input_schema = crate::tools::schema::input::<SummarizeProjectStatusParams>(),
        output_schema = crate::tools::schema::output::<ProjectStatusOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn summarize_project_status(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<SummarizeProjectStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let days = params.days.unwrap_or(DEFAULT_SUMMARY_DAYS).max(1);
        let project_ident = ProjectIdent::Id(ProjectId(params.project_id));
        let scoped = self.scoped(&ctx)?;

        let now = Utc::now();
        let cutoff: DateTime<Utc> = now
            .checked_sub_signed(Duration::days(i64::from(days)))
            .unwrap_or(now);
        let cutoff_filter = format!(">={}", cutoff.format("%Y-%m-%d"));

        let sample_query = IssueQuery {
            project: Some(project_ident.clone()),
            status: Some(StatusFilter::All),
            sort: Some("updated_on:desc".to_string()),
            ..IssueQuery::default()
        };
        let open_query = IssueQuery {
            project: Some(project_ident.clone()),
            status: Some(StatusFilter::Open),
            ..IssueQuery::default()
        };
        let closed_query = IssueQuery {
            project: Some(project_ident.clone()),
            status: Some(StatusFilter::Closed),
            ..IssueQuery::default()
        };
        let mut created_query = IssueQuery {
            project: Some(project_ident.clone()),
            status: Some(StatusFilter::All),
            ..IssueQuery::default()
        };
        created_query
            .extra
            .insert("created_on".to_string(), cutoff_filter.clone());
        let mut updated_query = IssueQuery {
            project: Some(project_ident.clone()),
            status: Some(StatusFilter::All),
            ..IssueQuery::default()
        };
        updated_query.updated_on = Some(cutoff_filter);

        // Concurrent, not spawned: `Scoped<'a>` borrows the credential, so
        // these futures are not `'static` and cannot go through
        // `tokio::task::JoinSet` — `try_join!` gives the same bounded,
        // countable fan-out without that requirement.
        let result = tokio::try_join!(
            scoped.get_project(&project_ident, &[]),
            scoped.list_issues_page(&sample_query, SUMMARY_SAMPLE_LIMIT, 0),
            scoped.list_issues_page(&open_query, 1, 0),
            scoped.list_issues_page(&closed_query, 1, 0),
            scoped.list_issues_page(&created_query, 1, 0),
            scoped.list_issues_page(&updated_query, 1, 0),
        );
        let (project, sample, open, closed, created, updated) = match result {
            Ok(t) => t,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let sample_size = u64::try_from(sample.items.len()).unwrap_or(u64::MAX);
        let status_breakdown = bucket_counts(sample.items.iter().map(|i| i.status.name.clone()))
            .into_iter()
            .map(|(name, count)| BreakdownEntry { name, count })
            .collect();
        let priority_breakdown =
            bucket_counts(sample.items.iter().map(|i| i.priority.name.clone()))
                .into_iter()
                .map(|(name, count)| BreakdownEntry { name, count })
                .collect();
        let assignee_breakdown = bucket_counts(sample.items.iter().map(|i| {
            i.assigned_to
                .as_ref()
                .map_or_else(|| "Unassigned".to_string(), |a| a.name.clone())
        }))
        .into_iter()
        .map(|(name, count)| BreakdownEntry {
            name: if name == "Unassigned" {
                name
            } else {
                boundary.wrap("issue.assigned_to.name", &name)
            },
            count,
        })
        .collect();

        let output = ProjectStatusOutput {
            project_id: project.id,
            project_name: boundary.wrap("project.name", &project.name),
            analysis_period_days: days,
            recent_activity: RecentActivity {
                created_count: created.total_count,
                updated_count: updated.total_count,
            },
            totals: ProjectTotals {
                total: sample.total_count,
                open: open.total_count,
                closed: closed.total_count,
            },
            status_breakdown,
            priority_breakdown,
            assignee_breakdown,
            sample_size,
            sample_truncated: sample_size < sample.total_count,
        };
        Ok(output::ok(&output, self.output_caps()))
    }

    /// `GET /projects/{id}/versions.json`. Always returns every version —
    /// the endpoint has no server-side status filter — so `status_filter`
    /// is applied client-side.
    #[tool(
        description = "List versions (roadmap milestones) for a Redmine project. Use this to discover a target version's id before filtering issues by fixed_version_id or calling manage_redmine_version. An empty list means the project has no versions configured — do not retry with the same arguments.",
        input_schema = crate::tools::schema::input::<ListRedmineVersionsParams>(),
        output_schema = crate::tools::schema::output::<VersionsOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_redmine_versions(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ListRedmineVersionsParams>,
    ) -> Result<CallToolResult, McpError> {
        let project_ident = resolve_project_ref(params.project_id)?;
        let scoped = self.scoped(&ctx)?;
        let versions = match scoped.list_versions(&project_ident).await {
            Ok(versions) => versions,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let versions = versions
            .iter()
            .filter(|v| {
                params
                    .status_filter
                    .is_none_or(|filter| v.status == filter.as_str())
            })
            .map(|v| version_out(&boundary, v))
            .collect();

        Ok(output::ok(&VersionsOutput { versions }, self.output_caps()))
    }

    /// `POST /projects/{id}/versions.json`, `PUT`/`DELETE /versions/{id}.json`.
    #[tool(
        description = "Create, update, or delete a Redmine version (roadmap milestone). Use this when the user wants to add, change, or remove a milestone. action=\"create\" needs project_id and name; \"update\"/\"delete\" need version_id (find one via list_redmine_versions). Blocked entirely in read-only mode.",
        input_schema = crate::tools::schema::input::<ManageRedmineVersionParams>(),
        output_schema = crate::tools::schema::output::<ManageVersionOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn manage_redmine_version(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ManageRedmineVersionParams>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let boundary = Boundary::new();

        match params.action {
            ManageVersionAction::Create => {
                let project_ref = params.project_id.ok_or_else(|| {
                    McpError::invalid_params("project_id is required for action=\"create\"", None)
                })?;
                let project_ident = resolve_project_ref(project_ref)?;
                let name = params.name.clone().ok_or_else(|| {
                    McpError::invalid_params("name is required for action=\"create\"", None)
                })?;
                let write = VersionWrite {
                    name: Some(name),
                    description: params.description.clone(),
                    status: params.status.map(Into::into),
                    due_date: params.due_date,
                    sharing: params.sharing.map(Into::into),
                    wiki_page_title: params.wiki_page_title.clone(),
                };
                let version = match scoped.create_version(&project_ident, &write).await {
                    Ok(version) => version,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageVersionOutput {
                        success: true,
                        version: Some(version_out(&boundary, &version)),
                        deleted_version_id: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageVersionAction::Update => {
                let version_id = params.version_id.ok_or_else(|| {
                    McpError::invalid_params("version_id is required for action=\"update\"", None)
                })?;
                let write = VersionWrite {
                    name: params.name.clone(),
                    description: params.description.clone(),
                    status: params.status.map(Into::into),
                    due_date: params.due_date,
                    sharing: params.sharing.map(Into::into),
                    wiki_page_title: params.wiki_page_title.clone(),
                };
                let version = match scoped.update_version(VersionId(version_id), &write).await {
                    Ok(version) => version,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageVersionOutput {
                        success: true,
                        version: Some(version_out(&boundary, &version)),
                        deleted_version_id: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageVersionAction::Delete => {
                let version_id = params.version_id.ok_or_else(|| {
                    McpError::invalid_params("version_id is required for action=\"delete\"", None)
                })?;
                match scoped.delete_version(VersionId(version_id)).await {
                    Ok(()) => {}
                    Err(e) => return Ok(to_tool_error(e)),
                }
                Ok(output::ok(
                    &ManageVersionOutput {
                        success: true,
                        version: None,
                        deleted_version_id: Some(version_id),
                    },
                    self.output_caps(),
                ))
            }
        }
    }

    /// `GET /projects/{id}/memberships.json`, auto-paged.
    #[tool(
        description = "List all members (users and groups) of a Redmine project along with their assigned roles. Use this to see who has access to a project, or before manage_project_member to find a membership_id to update or remove.",
        input_schema = crate::tools::schema::input::<ProjectIdParams>(),
        output_schema = crate::tools::schema::output::<MembershipsOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_project_members(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ProjectIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let project_ident = resolve_project_ref(params.project_id)?;
        let scoped = self.scoped(&ctx)?;
        let page = match scoped.list_memberships(&project_ident).await {
            Ok(page) => page,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let pagination = Pagination::from_page(&page);
        let memberships = page
            .items
            .iter()
            .map(|m| membership_out(&boundary, m))
            .collect();

        Ok(output::ok(
            &MembershipsOutput {
                memberships,
                pagination,
            },
            self.output_caps(),
        ))
    }

    /// `GET /roles.json`. Unlike `list_redmine_users`, this is **not**
    /// admin-gated on Redmine's side.
    #[tool(
        description = "List all roles defined in the Redmine instance (id and name only). Call this before manage_project_member(action=\"add\"|\"update\") to discover valid role_ids — role ids vary between Redmine instances and must not be guessed. Unlike list_redmine_users, this does not require an admin credential.",
        output_schema = crate::tools::schema::output::<RolesOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_redmine_roles(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let roles = match scoped.list_roles().await {
            Ok(roles) => roles,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let roles = roles
            .into_iter()
            .map(|r| RoleOut {
                id: r.id,
                name: r.name,
            })
            .collect();
        Ok(output::ok(&RolesOutput { roles }, self.output_caps()))
    }

    /// `GET /projects/{id}.json?include=enabled_modules`.
    #[tool(
        description = "Retrieve the list of enabled modules for a project (e.g. issue_tracking, time_tracking, wiki, repository). Use this to check whether a feature (like time tracking or the wiki) is available in a project before calling a module-specific tool.",
        input_schema = crate::tools::schema::input::<ProjectIdParams>(),
        output_schema = crate::tools::schema::output::<ProjectModulesOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn get_project_modules(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ProjectIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let project_ident = resolve_project_ref(params.project_id)?;
        let scoped = self.scoped(&ctx)?;
        let project = match scoped
            .get_project(&project_ident, &[ProjectInclude::EnabledModules])
            .await
        {
            Ok(project) => project,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let enabled_modules = project
            .enabled_modules
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.name)
            .collect();

        Ok(output::ok(
            &ProjectModulesOutput {
                project_id: project.id,
                project_name: boundary.wrap("project.name", &project.name),
                enabled_modules,
            },
            self.output_caps(),
        ))
    }

    /// `POST /projects/{id}/memberships.json`, `PUT`/`DELETE /memberships/{id}.json`.
    /// Redmine's API carries a group id through the same `user_id` wire
    /// field a user id goes through — there is no separate `group_id` field
    /// on the wire.
    #[tool(
        description = "Add, update, or remove a Redmine project membership. Use this to grant or change project access. action=\"add\" needs project_id, one of user_id/group_id, and role_ids; \"update\" needs membership_id and role_ids; \"remove\" needs membership_id (use list_redmine_roles first to find valid role_ids). Blocked in read-only mode; inherited memberships must be removed from the parent project instead.",
        input_schema = crate::tools::schema::input::<ManageProjectMemberParams>(),
        output_schema = crate::tools::schema::output::<ManageMemberOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn manage_project_member(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ManageProjectMemberParams>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let boundary = Boundary::new();

        match params.action {
            ManageMemberAction::Add => {
                let project_ref = params.project_id.ok_or_else(|| {
                    McpError::invalid_params("project_id is required for action=\"add\"", None)
                })?;
                let project_ident = resolve_project_ref(project_ref)?;
                let principal_id = match (params.user_id, params.group_id) {
                    (Some(user_id), None) => user_id,
                    (None, Some(group_id)) => group_id,
                    (Some(_), Some(_)) => {
                        return Err(McpError::invalid_params(
                            "exactly one of user_id or group_id is required for action=\"add\", not both",
                            None,
                        ));
                    }
                    (None, None) => {
                        return Err(McpError::invalid_params(
                            "exactly one of user_id or group_id is required for action=\"add\"",
                            None,
                        ));
                    }
                };
                let role_ids = params
                    .role_ids
                    .clone()
                    .filter(|r| !r.is_empty())
                    .ok_or_else(|| {
                        McpError::invalid_params(
                            "a non-empty role_ids is required for action=\"add\"",
                            None,
                        )
                    })?;
                let new = MembershipCreate {
                    user_id: principal_id,
                    role_ids,
                };
                let membership = match scoped.create_membership(&project_ident, &new).await {
                    Ok(membership) => membership,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageMemberOutput {
                        success: true,
                        membership: Some(membership_out(&boundary, &membership)),
                        deleted_membership_id: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageMemberAction::Update => {
                let membership_id = params.membership_id.ok_or_else(|| {
                    McpError::invalid_params(
                        "membership_id is required for action=\"update\"",
                        None,
                    )
                })?;
                let role_ids = params
                    .role_ids
                    .clone()
                    .filter(|r| !r.is_empty())
                    .ok_or_else(|| {
                        McpError::invalid_params(
                            "a non-empty role_ids is required for action=\"update\"",
                            None,
                        )
                    })?;
                let patch = MembershipUpdate { role_ids };
                let membership = match scoped
                    .update_membership(MembershipId(membership_id), &patch)
                    .await
                {
                    Ok(membership) => membership,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageMemberOutput {
                        success: true,
                        membership: Some(membership_out(&boundary, &membership)),
                        deleted_membership_id: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageMemberAction::Remove => {
                let membership_id = params.membership_id.ok_or_else(|| {
                    McpError::invalid_params(
                        "membership_id is required for action=\"remove\"",
                        None,
                    )
                })?;
                match scoped.delete_membership(MembershipId(membership_id)).await {
                    Ok(()) => {}
                    Err(e) => return Ok(to_tool_error(e)),
                }
                Ok(output::ok(
                    &ManageMemberOutput {
                        success: true,
                        membership: None,
                        deleted_membership_id: Some(membership_id),
                    },
                    self.output_caps(),
                ))
            }
        }
    }
}

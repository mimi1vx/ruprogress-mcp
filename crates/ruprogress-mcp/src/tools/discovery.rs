//! Discovery / enumeration tools: `list_redmine_trackers`,
//! `list_project_trackers`, `list_redmine_issue_statuses`,
//! `list_redmine_issue_priorities`, `list_redmine_users`, `get_current_user`,
//! `list_redmine_queries`. The reference groups `get_current_user` here too,
//! so it lives in this module rather than a one-tool `users.rs`.

use std::str::FromStr as _;

use chrono::{DateTime, Utc};
use redmine_client::{ProjectId, ProjectIdent, ProjectIdentifier};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::to_tool_error;
use crate::render::Boundary;
use crate::server::RedmineMcp;
use crate::tools::output::{self, ErrorCode, Pagination};

// --- list_redmine_trackers ---

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct TrackerOut {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct TrackersOutput {
    pub(crate) trackers: Vec<TrackerOut>,
}

// --- list_project_trackers ---

/// A project's numeric id or slug identifier, as sent by the model.
/// Converted to a validated [`ProjectIdent`] on the first line of the tool
/// (D5) — this type itself performs no validation.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub(crate) enum ProjectRef {
    /// The project's numeric id, e.g. `5`.
    Id(u64),
    /// The project's slug identifier, e.g. `"my-project"`.
    Identifier(String),
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListProjectTrackersParams {
    /// The project to list enabled trackers for: numeric id or slug
    /// identifier. Prefer `list_redmine_trackers` when no project is known.
    pub(crate) project_id: ProjectRef,
}

/// Convert a [`ProjectRef`] (D5's untagged `integer | string` union) to a
/// validated [`ProjectIdent`], on the first line of every tool that takes a
/// `project_id` parameter — shared by `discovery.rs` and `projects.rs`. An
/// invalid slug identifier is an **argument** error (`McpError`), not a tool
/// result: the model gave us something that cannot be a project.
pub(crate) fn resolve_project_ref(r: ProjectRef) -> Result<ProjectIdent, McpError> {
    match r {
        ProjectRef::Id(id) => Ok(ProjectIdent::Id(ProjectId(id))),
        ProjectRef::Identifier(s) => ProjectIdentifier::from_str(&s)
            .map(ProjectIdent::Identifier)
            .map_err(|e| McpError::invalid_params(e.to_string(), None)),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ProjectTrackerOut {
    pub(crate) id: u64,
    pub(crate) name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ProjectTrackersOutput {
    pub(crate) trackers: Vec<ProjectTrackerOut>,
}

// --- list_redmine_issue_statuses ---

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IssueStatusOut {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) is_closed: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IssueStatusesOutput {
    pub(crate) issue_statuses: Vec<IssueStatusOut>,
}

// --- list_redmine_issue_priorities ---

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IssuePriorityOut {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) active: Option<bool>,
    pub(crate) is_default: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IssuePrioritiesOutput {
    pub(crate) issue_priorities: Vec<IssuePriorityOut>,
}

// --- list_redmine_users ---

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListRedmineUsersParams {
    /// Filter by name: matches login, firstname, lastname, or a
    /// "firstname lastname" pair.
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// Restrict to members of this group.
    #[serde(default)]
    pub(crate) group_id: Option<u64>,
    /// Page size, clamped to 1-100. Defaults to 25.
    #[serde(default)]
    pub(crate) limit: Option<u32>,
    /// Offset of the first result. Defaults to 0.
    #[serde(default)]
    pub(crate) offset: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct UserOut {
    pub(crate) id: u64,
    pub(crate) login: Option<String>,
    pub(crate) firstname: String,
    pub(crate) lastname: String,
    pub(crate) mail: Option<String>,
    pub(crate) created_on: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct UsersOutput {
    pub(crate) users: Vec<UserOut>,
    pub(crate) pagination: Pagination,
}

const USERS_MIN_LIMIT: u32 = 1;
const USERS_MAX_LIMIT: u32 = 100;
const USERS_DEFAULT_LIMIT: u32 = 25;

/// Clamp to [1, 100] (E4): a value outside the range is silently corrected
/// rather than rejected, since the model can't act on a rejection any more
/// usefully than on the clamp — the effective value is echoed back in
/// `pagination.limit`.
fn clamp_users_limit(limit: Option<u32>) -> u32 {
    limit
        .unwrap_or(USERS_DEFAULT_LIMIT)
        .clamp(USERS_MIN_LIMIT, USERS_MAX_LIMIT)
}

// --- get_current_user ---

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct CurrentUserOutput {
    pub(crate) id: u64,
    pub(crate) login: Option<String>,
    pub(crate) firstname: String,
    pub(crate) lastname: String,
    pub(crate) mail: Option<String>,
    pub(crate) admin: Option<bool>,
    pub(crate) created_on: DateTime<Utc>,
    pub(crate) last_login_on: Option<DateTime<Utc>>,
}

// --- list_redmine_queries ---

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct SavedQueryOut {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) is_public: Option<bool>,
    pub(crate) project_id: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct SavedQueriesOutput {
    pub(crate) queries: Vec<SavedQueryOut>,
    pub(crate) pagination: Pagination,
}

#[tool_router(router = discovery_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// `GET /trackers.json`. Instance-wide tracker list — a project's
    /// enabled trackers may be a subset; use `list_project_trackers` instead
    /// when a project is known.
    #[tool(
        description = "List every tracker (Bug, Feature, ...) configured on the Redmine instance. Use this to resolve a tracker name to an id before creating an issue, when no project is known yet. Prefer list_project_trackers when a project id is available, since a project can restrict which trackers it accepts. An empty list means no trackers are configured — do not retry with the same arguments.",
        output_schema = crate::tools::schema::output::<TrackersOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_redmine_trackers(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let trackers = match scoped.list_trackers().await {
            Ok(trackers) => trackers,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let trackers = trackers
            .iter()
            .map(|t| TrackerOut {
                id: t.id,
                name: boundary.wrap("tracker.name", &t.name),
                description: t
                    .description
                    .as_deref()
                    .map(|d| boundary.wrap("tracker.description", d)),
            })
            .collect();

        Ok(output::ok(&TrackersOutput { trackers }, self.output_caps()))
    }

    /// `GET /projects/{id}.json?include=trackers` — the trackers *enabled
    /// for this project*, which a project's settings can restrict to a
    /// subset of `list_redmine_trackers`'s instance-wide list.
    #[tool(
        description = "List the trackers enabled for a specific project (numeric id or slug identifier). Use this instead of list_redmine_trackers whenever a project is known, since a project's settings can restrict which trackers it accepts. An empty list means no trackers are enabled for this project — do not retry with the same arguments.",
        input_schema = crate::tools::schema::input::<ListProjectTrackersParams>(),
        output_schema = crate::tools::schema::output::<ProjectTrackersOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_project_trackers(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ListProjectTrackersParams>,
    ) -> Result<CallToolResult, McpError> {
        let project_ident = resolve_project_ref(params.project_id)?;

        let scoped = self.scoped(&ctx)?;
        let project = match scoped
            .get_project(
                &project_ident,
                &[redmine_client::model::project::ProjectInclude::Trackers],
            )
            .await
        {
            Ok(project) => project,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let trackers = project
            .trackers
            .unwrap_or_default()
            .iter()
            .map(|t| ProjectTrackerOut {
                id: t.id,
                name: boundary.wrap("tracker.name", &t.name),
            })
            .collect();

        Ok(output::ok(
            &ProjectTrackersOutput { trackers },
            self.output_caps(),
        ))
    }

    /// `GET /issue_statuses.json`.
    #[tool(
        description = "List every issue status (New, In Progress, Closed, ...) configured on the Redmine instance, including which ones count as closed. Use this to resolve a status name to an id before filtering or updating issues. An empty list would mean the instance has none configured — do not retry with the same arguments.",
        output_schema = crate::tools::schema::output::<IssueStatusesOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_redmine_issue_statuses(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let statuses = match scoped.list_issue_statuses().await {
            Ok(statuses) => statuses,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let issue_statuses = statuses
            .iter()
            .map(|s| IssueStatusOut {
                id: s.id,
                name: boundary.wrap("issue_status.name", &s.name),
                is_closed: s.is_closed,
            })
            .collect();

        Ok(output::ok(
            &IssueStatusesOutput { issue_statuses },
            self.output_caps(),
        ))
    }

    /// `GET /enumerations/issue_priorities.json`.
    #[tool(
        description = "List every issue priority (Low, Normal, High, ...) configured on the Redmine instance. Use this to resolve a priority name to an id before creating or updating an issue. An empty list means no priorities are configured — do not retry with the same arguments.",
        output_schema = crate::tools::schema::output::<IssuePrioritiesOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_redmine_issue_priorities(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let priorities = match scoped.list_issue_priorities().await {
            Ok(priorities) => priorities,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let issue_priorities = priorities
            .iter()
            .map(|p| IssuePriorityOut {
                id: p.id,
                name: boundary.wrap("issue_priority.name", &p.name),
                active: p.active,
                is_default: p.is_default,
            })
            .collect();

        Ok(output::ok(
            &IssuePrioritiesOutput { issue_priorities },
            self.output_caps(),
        ))
    }

    /// `GET /users.json`. **Admin-only** on Redmine's side: a non-admin
    /// credential gets a 403, surfaced as `code: "FORBIDDEN"`.
    #[tool(
        description = "List Redmine user accounts, optionally filtered by name or group. Requires an admin credential. Use this to resolve a user's name to an id before assigning an issue. If this returns a FORBIDDEN error, the credential is not an admin — do not retry; call get_current_user to check your own identity, or ask the user for an admin account.",
        input_schema = crate::tools::schema::input::<ListRedmineUsersParams>(),
        output_schema = crate::tools::schema::output::<UsersOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_redmine_users(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ListRedmineUsersParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = clamp_users_limit(params.limit);
        let offset = params.offset.unwrap_or(0);
        let query = redmine_client::model::user::UserQuery {
            name: params.name,
            group_id: params.group_id,
            status: None,
        };

        let scoped = self.scoped(&ctx)?;
        let page = match scoped.list_users(&query, limit, offset).await {
            Ok(page) => page,
            Err(redmine_client::Error::Forbidden) => {
                return Ok(output::err(
                    ErrorCode::Forbidden,
                    "the configured Redmine credential is not an administrator (403 forbidden); list_redmine_users requires admin privileges",
                    Some(
                        "do not retry; call get_current_user to check your own identity, or ask the user for an account with admin privileges",
                    ),
                ));
            }
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let pagination = Pagination::from_page(&page);
        let users = page
            .items
            .iter()
            .map(|u| UserOut {
                id: u.id,
                login: u.login.clone(),
                firstname: boundary.wrap("user.firstname", &u.firstname),
                lastname: boundary.wrap("user.lastname", &u.lastname),
                mail: u.mail.clone(),
                created_on: u.created_on,
            })
            .collect();

        Ok(output::ok(
            &UsersOutput { users, pagination },
            self.output_caps(),
        ))
    }

    /// Resolves to `GET /my/account.json`; works for any authenticated user,
    /// not just admins.
    #[tool(
        description = "Retrieve the currently authenticated user's profile (id, login, name, mail, admin flag). Use this to resolve \"me\" or to check whether the credential is an admin before calling admin-only tools like list_redmine_users.",
        output_schema = crate::tools::schema::output::<CurrentUserOutput>(),
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
            admin: user.admin,
            created_on: user.created_on,
            last_login_on: user.last_login_on,
        };
        Ok(output::ok(&output, self.output_caps()))
    }

    /// `GET /queries.json`, auto-paged. Redmine's REST API has no
    /// create/update/delete for saved queries: this is read-only by nature,
    /// and there is no `manage_redmine_query` tool.
    #[tool(
        description = "List the current user's saved (custom) issue queries. Redmine has no API to create, update, or delete saved queries, so this is the only query-related tool — do not look for a manage_redmine_query tool. Use this to resolve a saved query's name to an id. An empty list means the user has no saved queries.",
        output_schema = crate::tools::schema::output::<SavedQueriesOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_redmine_queries(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let page = match scoped.list_saved_queries().await {
            Ok(page) => page,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let pagination = Pagination::from_page(&page);
        let queries = page
            .items
            .iter()
            .map(|q| SavedQueryOut {
                id: q.id,
                name: boundary.wrap("query.name", &q.name),
                is_public: q.is_public,
                project_id: q.project_id,
            })
            .collect();

        Ok(output::ok(
            &SavedQueriesOutput {
                queries,
                pagination,
            },
            self.output_caps(),
        ))
    }
}

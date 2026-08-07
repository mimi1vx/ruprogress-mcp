//! Time-tracking tools (4d): `list_time_entries`, `manage_time_entry`,
//! `list_time_entry_activities`, `import_time_entries`. See
//! `plans/phase-4d-time.md`.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use redmine_client::model::issue::UserFilter;
use redmine_client::model::project::ProjectInclude;
use redmine_client::model::time_entry::{
    TimeEntry, TimeEntryCreate, TimeEntryQuery, TimeEntryUpdate,
};
use redmine_client::{IssueId, TimeEntryId, UserId};
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

// --- shared shapes ---

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IdNameOut {
    pub(crate) id: u64,
    pub(crate) name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct TimeEntryOut {
    pub(crate) id: u64,
    pub(crate) project: IdNameOut,
    /// The issue this time was logged against, if any. Redmine sends no
    /// `name` alongside a time entry's issue reference, unlike `project`/
    /// `user`/`activity` — hence a bare id rather than an `IdNameOut`.
    pub(crate) issue_id: Option<u64>,
    pub(crate) user: IdNameOut,
    pub(crate) activity: IdNameOut,
    pub(crate) hours: f64,
    pub(crate) comments: Option<String>,
    pub(crate) spent_on: NaiveDate,
    pub(crate) created_on: DateTime<Utc>,
    pub(crate) updated_on: DateTime<Utc>,
}

fn time_entry_out(boundary: &Boundary, e: &TimeEntry) -> TimeEntryOut {
    TimeEntryOut {
        id: e.id,
        project: IdNameOut {
            id: e.project.id,
            name: boundary.wrap("time_entry.project.name", &e.project.name),
        },
        issue_id: e.issue.as_ref().map(|i| i.id),
        user: IdNameOut {
            id: e.user.id,
            name: boundary.wrap("time_entry.user.name", &e.user.name),
        },
        activity: IdNameOut {
            id: e.activity.id,
            name: boundary.wrap("time_entry.activity.name", &e.activity.name),
        },
        hours: e.hours,
        comments: e
            .comments
            .as_deref()
            .map(|c| boundary.wrap("time_entry.comments", c)),
        spent_on: e.spent_on,
        created_on: e.created_on,
        updated_on: e.updated_on,
    }
}

/// D5: a user id or the literal string `"me"`, as sent by the model. Kept
/// local to this module rather than shared with `tools/issues.rs`'s
/// `AssignedToRef`: same shape, different underlying Redmine parameter
/// (`user_id` here vs. `assigned_to_id` there) and only one call site each —
/// see `plans/phase-4d-time.md` decision H3.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub(crate) enum UserRef {
    /// A specific user id, e.g. `5`.
    Id(u64),
    /// The literal string `"me"` — resolves to the caller's own user id.
    Literal(String),
}

fn resolve_user_ref(r: UserRef) -> Result<UserFilter, McpError> {
    match r {
        UserRef::Id(id) => Ok(UserFilter::Id(UserId(id))),
        UserRef::Literal(s) if s == "me" => Ok(UserFilter::Me),
        UserRef::Literal(other) => Err(McpError::invalid_params(
            format!("user_id must be an integer or the literal \"me\", got {other:?}"),
            None,
        )),
    }
}

/// H1: translate the two typed dates into Redmine's single `spent_on`
/// operator-syntax filter. `None` when neither is given (no filter sent).
fn build_spent_on(from_date: Option<NaiveDate>, to_date: Option<NaiveDate>) -> Option<String> {
    match (from_date, to_date) {
        (Some(from), Some(to)) => Some(format!("><{from}|{to}")),
        (Some(from), None) => Some(format!(">={from}")),
        (None, Some(to)) => Some(format!("<={to}")),
        (None, None) => None,
    }
}

/// Extract `(code, message)` from `to_tool_error`'s envelope, for
/// `import_time_entries`'s per-entry error reporting — reuses the same
/// Redmine-error-to-text mapping every other tool uses (D4), rather than
/// inventing a second one for the batch case.
fn describe_error(e: redmine_client::Error) -> String {
    let result = to_tool_error(e);
    let structured = result.structured_content.unwrap_or_default();
    let code = structured
        .get("code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("UNEXPECTED_RESPONSE");
    let message = structured
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown error");
    format!("{code}: {message}")
}

// --- list_time_entries ---

const TIME_ENTRIES_MIN_LIMIT: u32 = 1;
const TIME_ENTRIES_MAX_LIMIT: u32 = 100;
const TIME_ENTRIES_DEFAULT_LIMIT: u32 = 25;

/// Clamp to [1, 100] (matching the reference contract's own max, which is
/// tighter than `list_redmine_issues`'s 1000): a value outside the range is
/// silently corrected, echoed back in `pagination.limit`.
fn clamp_time_entries_limit(limit: Option<u32>) -> u32 {
    limit
        .unwrap_or(TIME_ENTRIES_DEFAULT_LIMIT)
        .clamp(TIME_ENTRIES_MIN_LIMIT, TIME_ENTRIES_MAX_LIMIT)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListTimeEntriesParams {
    /// Restrict to one project: numeric id or slug identifier.
    #[serde(default)]
    pub(crate) project_id: Option<ProjectRef>,
    /// Restrict to one issue.
    #[serde(default)]
    pub(crate) issue_id: Option<u64>,
    /// Restrict to one user: a numeric user id, or `"me"` for the
    /// credential's own user.
    #[serde(default)]
    pub(crate) user_id: Option<UserRef>,
    /// Only entries spent on or after this date (`YYYY-MM-DD`).
    #[serde(default)]
    pub(crate) from_date: Option<NaiveDate>,
    /// Only entries spent on or before this date (`YYYY-MM-DD`).
    #[serde(default)]
    pub(crate) to_date: Option<NaiveDate>,
    /// Page size, clamped to 1-100. Defaults to 25.
    #[serde(default)]
    pub(crate) limit: Option<u32>,
    /// Offset of the first result. Defaults to 0.
    #[serde(default)]
    pub(crate) offset: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct TimeEntriesOutput {
    pub(crate) time_entries: Vec<TimeEntryOut>,
    pub(crate) pagination: Pagination,
}

// --- manage_time_entry ---

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ManageTimeEntryAction {
    Create,
    Update,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManageTimeEntryParams {
    /// Operation to perform.
    pub(crate) action: ManageTimeEntryAction,
    /// Hours spent. Required (and must be positive) for `action = "create"`;
    /// optional (but must be positive if given) for `action = "update"`.
    #[serde(default)]
    pub(crate) hours: Option<f64>,
    /// Project id or identifier. Required for `action = "create"` if
    /// `issue_id` is not provided.
    #[serde(default)]
    pub(crate) project_id: Option<ProjectRef>,
    /// Required for `action = "create"` if `project_id` is not provided.
    #[serde(default)]
    pub(crate) issue_id: Option<u64>,
    /// Log time on behalf of this user (`action = "create"` only). Requires
    /// the `log_time_for_other_users` permission.
    #[serde(default)]
    pub(crate) user_id: Option<u64>,
    /// Entry id to update. Required for `action = "update"`.
    #[serde(default)]
    pub(crate) time_entry_id: Option<u64>,
    #[serde(default)]
    pub(crate) activity_id: Option<u64>,
    /// Description. An empty string clears the field on `action = "update"`;
    /// omitting this field leaves it untouched.
    #[serde(default)]
    pub(crate) comments: Option<String>,
    /// `YYYY-MM-DD`. Defaults to today on create if omitted.
    #[serde(default)]
    pub(crate) spent_on: Option<NaiveDate>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ManageTimeEntryOutput {
    pub(crate) success: bool,
    pub(crate) time_entry: TimeEntryOut,
}

// --- list_time_entry_activities ---

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListTimeEntryActivitiesParams {
    /// When given, returns this project's enabled activities instead of the
    /// instance-wide list. The project-scoped shape never carries
    /// `active`/`is_default` (Redmine only exposes those on the global
    /// enumeration) — those fields are `null` in that case, not fabricated.
    #[serde(default)]
    pub(crate) project_id: Option<ProjectRef>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct TimeEntryActivityOut {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) active: Option<bool>,
    pub(crate) is_default: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct TimeEntryActivitiesOutput {
    pub(crate) time_entry_activities: Vec<TimeEntryActivityOut>,
}

// --- import_time_entries ---

/// Capped per the reference contract: "split larger imports into multiple
/// invocations" (H7). Enforced as an argument error before any request is
/// sent, never a silent truncation.
const IMPORT_MAX_ENTRIES: usize = 500;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportEntryParams {
    pub(crate) hours: f64,
    #[serde(default)]
    pub(crate) project_id: Option<ProjectRef>,
    #[serde(default)]
    pub(crate) issue_id: Option<u64>,
    #[serde(default)]
    pub(crate) user_id: Option<u64>,
    #[serde(default)]
    pub(crate) activity_id: Option<u64>,
    #[serde(default)]
    pub(crate) comments: Option<String>,
    #[serde(default)]
    pub(crate) spent_on: Option<NaiveDate>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportTimeEntriesParams {
    /// At most 500 entries per call.
    pub(crate) entries: Vec<ImportEntryParams>,
    /// Abort on the first error. Default `false`: keep attempting every
    /// entry and report per-entry outcomes.
    #[serde(default)]
    pub(crate) stop_on_error: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ImportEntryResult {
    pub(crate) index: usize,
    /// `false` when `stop_on_error` fired before this entry was reached —
    /// never a fabricated failure for an entry that was never attempted.
    pub(crate) attempted: bool,
    pub(crate) success: bool,
    pub(crate) time_entry: Option<TimeEntryOut>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ImportTimeEntriesOutput {
    pub(crate) total: usize,
    pub(crate) attempted: usize,
    pub(crate) succeeded: usize,
    pub(crate) failed: usize,
    pub(crate) results: Vec<ImportEntryResult>,
}

#[tool_router(router = time_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// `GET /time_entries.json`, a single explicit page.
    #[tool(
        description = "List logged time entries, optionally filtered by project, issue, user, or date range. Use this to review time already logged before summarizing or exporting it. from_date/to_date are translated into Redmine's own spent_on filter syntax. An empty list means no matching entries — do not retry with the same arguments.",
        input_schema = crate::tools::schema::input::<ListTimeEntriesParams>(),
        output_schema = crate::tools::schema::output::<TimeEntriesOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_time_entries(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ListTimeEntriesParams>,
    ) -> Result<CallToolResult, McpError> {
        let project_ident = params.project_id.map(resolve_project_ref).transpose()?;
        let user_filter = params.user_id.map(resolve_user_ref).transpose()?;
        let limit = clamp_time_entries_limit(params.limit);
        let offset = params.offset.unwrap_or(0);

        let query = TimeEntryQuery {
            project_id: project_ident,
            issue_id: params.issue_id.map(IssueId),
            user_id: user_filter,
            spent_on: build_spent_on(params.from_date, params.to_date),
            extra: BTreeMap::default(),
        };

        let scoped = self.scoped(&ctx)?;
        let page = match scoped.list_time_entries_page(&query, limit, offset).await {
            Ok(page) => page,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let pagination = Pagination::from_page(&page);
        let time_entries = page
            .items
            .iter()
            .map(|e| time_entry_out(&boundary, e))
            .collect();

        Ok(output::ok(
            &TimeEntriesOutput {
                time_entries,
                pagination,
            },
            self.output_caps(),
        ))
    }

    /// `POST /time_entries.json`, `PUT /time_entries/{id}.json` (followed by
    /// a `GET`, since Redmine's `PUT` answers `204 No Content`).
    #[tool(
        description = "Log or update a time entry against an issue or project. Use this when the user wants to record time spent. action=\"create\" needs hours and at least one of project_id/issue_id; \"update\" needs time_entry_id. Blocked entirely in read-only mode.",
        input_schema = crate::tools::schema::input::<ManageTimeEntryParams>(),
        output_schema = crate::tools::schema::output::<ManageTimeEntryOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn manage_time_entry(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ManageTimeEntryParams>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let boundary = Boundary::new();

        match params.action {
            ManageTimeEntryAction::Create => {
                let hours = params.hours.ok_or_else(|| {
                    McpError::invalid_params("hours is required for action=\"create\"", None)
                })?;
                if hours <= 0.0 {
                    return Err(McpError::invalid_params("hours must be positive", None));
                }
                if params.project_id.is_none() && params.issue_id.is_none() {
                    return Err(McpError::invalid_params(
                        "at least one of project_id or issue_id is required for action=\"create\"",
                        None,
                    ));
                }
                let project_id = params
                    .project_id
                    .clone()
                    .map(resolve_project_ref)
                    .transpose()?;
                let create = TimeEntryCreate {
                    issue_id: params.issue_id.map(IssueId),
                    project_id,
                    spent_on: params.spent_on,
                    hours,
                    activity_id: params.activity_id,
                    comments: params.comments.clone(),
                    user_id: params.user_id,
                };
                let entry = match scoped.create_time_entry(&create).await {
                    Ok(entry) => entry,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageTimeEntryOutput {
                        success: true,
                        time_entry: time_entry_out(&boundary, &entry),
                    },
                    self.output_caps(),
                ))
            }
            ManageTimeEntryAction::Update => {
                let time_entry_id = params.time_entry_id.ok_or_else(|| {
                    McpError::invalid_params(
                        "time_entry_id is required for action=\"update\"",
                        None,
                    )
                })?;
                if let Some(hours) = params.hours
                    && hours <= 0.0
                {
                    return Err(McpError::invalid_params("hours must be positive", None));
                }
                let patch = TimeEntryUpdate {
                    hours: params.hours,
                    activity_id: params.activity_id,
                    comments: params.comments.clone(),
                    spent_on: params.spent_on,
                };
                let entry = match scoped
                    .update_time_entry(TimeEntryId(time_entry_id), &patch)
                    .await
                {
                    Ok(entry) => entry,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageTimeEntryOutput {
                        success: true,
                        time_entry: time_entry_out(&boundary, &entry),
                    },
                    self.output_caps(),
                ))
            }
        }
    }

    /// `GET /enumerations/time_entry_activities.json` without `project_id`;
    /// `GET /projects/{id}.json?include=time_entry_activities` with one.
    #[tool(
        description = "List time-tracking activities (Development, QA, ...). Pass project_id to see only activities enabled for that project (Redmine 3.4.0+); without it, lists every activity defined on the instance, including active/is_default flags the project-scoped form does not carry. Use this to resolve an activity name to an id before manage_time_entry/import_time_entries.",
        input_schema = crate::tools::schema::input::<ListTimeEntryActivitiesParams>(),
        output_schema = crate::tools::schema::output::<TimeEntryActivitiesOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_time_entry_activities(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ListTimeEntryActivitiesParams>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let boundary = Boundary::new();

        let activities: Vec<TimeEntryActivityOut> = if let Some(project_ref) = params.project_id {
            let project_ident = resolve_project_ref(project_ref)?;
            let project = match scoped
                .get_project(&project_ident, &[ProjectInclude::TimeEntryActivities])
                .await
            {
                Ok(project) => project,
                Err(e) => return Ok(to_tool_error(e)),
            };
            project
                .time_entry_activities
                .unwrap_or_default()
                .into_iter()
                .map(|a| TimeEntryActivityOut {
                    id: a.id,
                    name: boundary.wrap("time_entry_activity.name", &a.name),
                    active: None,
                    is_default: None,
                })
                .collect()
        } else {
            let activities = match scoped.list_time_entry_activities().await {
                Ok(activities) => activities,
                Err(e) => return Ok(to_tool_error(e)),
            };
            activities
                .into_iter()
                .map(|a| TimeEntryActivityOut {
                    id: a.id,
                    name: boundary.wrap("time_entry_activity.name", &a.name),
                    active: a.active,
                    is_default: a.is_default,
                })
                .collect()
        };

        Ok(output::ok(
            &TimeEntryActivitiesOutput {
                time_entry_activities: activities,
            },
            self.output_caps(),
        ))
    }

    /// `POST /time_entries.json`, once per entry, sequentially. Never
    /// retried by `RetryPolicy` (POST is not retry-eligible), so a partial
    /// failure reflects Redmine's real response, not a masked transient
    /// error.
    #[tool(
        description = "Bulk-log up to 500 time entries in one call. Use this instead of repeated manage_time_entry calls when importing time from an external source. Each entry needs hours and at least one of project_id/issue_id. Continues past a failing entry by default and reports every outcome; stop_on_error=true halts at the first failure. Created entries are never rolled back. Blocked in read-only mode.",
        input_schema = crate::tools::schema::input::<ImportTimeEntriesParams>(),
        output_schema = crate::tools::schema::output::<ImportTimeEntriesOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn import_time_entries(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ImportTimeEntriesParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.entries.is_empty() {
            return Err(McpError::invalid_params("entries must not be empty", None));
        }
        if params.entries.len() > IMPORT_MAX_ENTRIES {
            return Err(McpError::invalid_params(
                format!(
                    "entries has {} items, over the {IMPORT_MAX_ENTRIES}-entry cap per call; split into multiple invocations",
                    params.entries.len()
                ),
                None,
            ));
        }

        // Validate every entry and resolve every project reference (D5)
        // before a single HTTP request is sent — a mid-batch argument error
        // must never leave some entries created and the rest rejected for a
        // reason the model could have fixed up front.
        let mut resolved_projects = Vec::with_capacity(params.entries.len());
        for (index, entry) in params.entries.iter().enumerate() {
            if entry.hours <= 0.0 {
                return Err(McpError::invalid_params(
                    format!("entries[{index}].hours must be positive"),
                    None,
                ));
            }
            if entry.project_id.is_none() && entry.issue_id.is_none() {
                return Err(McpError::invalid_params(
                    format!("entries[{index}] needs at least one of project_id or issue_id"),
                    None,
                ));
            }
            let project_ident = entry
                .project_id
                .clone()
                .map(resolve_project_ref)
                .transpose()?;
            resolved_projects.push(project_ident);
        }

        let scoped = self.scoped(&ctx)?;
        let boundary = Boundary::new();
        let mut results = Vec::with_capacity(params.entries.len());
        let mut succeeded: usize = 0;
        let mut failed: usize = 0;
        let mut stopped = false;

        for (index, (entry, project_ident)) in params
            .entries
            .iter()
            .zip(resolved_projects.iter())
            .enumerate()
        {
            if stopped {
                results.push(ImportEntryResult {
                    index,
                    attempted: false,
                    success: false,
                    time_entry: None,
                    error: None,
                });
                continue;
            }

            let create = TimeEntryCreate {
                issue_id: entry.issue_id.map(IssueId),
                project_id: project_ident.clone(),
                spent_on: entry.spent_on,
                hours: entry.hours,
                activity_id: entry.activity_id,
                comments: entry.comments.clone(),
                user_id: entry.user_id,
            };
            match scoped.create_time_entry(&create).await {
                Ok(created) => {
                    succeeded = succeeded.saturating_add(1);
                    results.push(ImportEntryResult {
                        index,
                        attempted: true,
                        success: true,
                        time_entry: Some(time_entry_out(&boundary, &created)),
                        error: None,
                    });
                }
                Err(e) => {
                    failed = failed.saturating_add(1);
                    results.push(ImportEntryResult {
                        index,
                        attempted: true,
                        success: false,
                        time_entry: None,
                        error: Some(describe_error(e)),
                    });
                    if params.stop_on_error {
                        stopped = true;
                    }
                }
            }
        }

        let attempted = succeeded.saturating_add(failed);
        Ok(output::ok(
            &ImportTimeEntriesOutput {
                total: params.entries.len(),
                attempted,
                succeeded,
                failed,
                results,
            },
            self.output_caps(),
        ))
    }
}

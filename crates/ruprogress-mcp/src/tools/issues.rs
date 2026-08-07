//! Issue read tools (4b-read): `get_redmine_issue`, `list_redmine_issues`,
//! `search_redmine_issues`, `list_subtasks`, `get_private_notes`. The
//! write/mixed issue tools (`create_redmine_issue`, `update_redmine_issue`,
//! `delete_redmine_issue`, `copy_issue`, `manage_issue_relation`,
//! `manage_issue_watcher`, `manage_issue_note`, `manage_issue_category`) land
//! in a later sub-phase (4b-write) — see `plans/phase-4b-issues.md`.
//!
//! `JournalOut` deliberately omits `details` (the field-change history
//! attached to a journal): no example in the reference contract renders it,
//! and an unbounded diff of e.g. a `description` change could itself blow
//! past the D9 byte cap. Revisit if a concrete need for it surfaces.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, NaiveDate, Utc};
use redmine_client::model::attachment::Attachment;
use redmine_client::model::custom_field::CustomFieldValue;
use redmine_client::model::issue::{
    Issue, IssueChild as ClientIssueChild, IssueChildLeaf as ClientIssueChildLeaf, IssueInclude,
    IssueQuery, StatusFilter, UserFilter,
};
use redmine_client::model::journal::Journal as ClientJournal;
use redmine_client::model::relation::IssueRelation as ClientIssueRelation;
use redmine_client::model::search::{SearchQuery, SearchScope};
use redmine_client::model::{CustomField, IdName};
use redmine_client::{IssueId, UserId};
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

// --- shared output shapes ---

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IdNameOut {
    pub(crate) id: u64,
    pub(crate) name: String,
}

fn id_name_out(boundary: &Boundary, kind: &str, v: &IdName) -> IdNameOut {
    IdNameOut {
        id: v.id,
        name: boundary.wrap(kind, &v.name),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IdOnlyOut {
    pub(crate) id: u64,
}

// --- shared `fields` selection (G5) ---

/// Field names the reference contract accepts for `list_redmine_issues` and
/// `search_redmine_issues`'s `fields` parameter, minus `id`/`tracker` (always
/// included, never filterable — see G5).
const OPTIONAL_ISSUE_FIELD_NAMES: &[&str] = &[
    "subject",
    "description",
    "project",
    "status",
    "priority",
    "author",
    "assigned_to",
    "created_on",
    "updated_on",
];

#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the reference contract's nine independent field-inclusion flags exactly"
)]
struct IssueFieldSet {
    subject: bool,
    description: bool,
    project: bool,
    status: bool,
    priority: bool,
    author: bool,
    assigned_to: bool,
    created_on: bool,
    updated_on: bool,
}

impl IssueFieldSet {
    const ALL: Self = Self {
        subject: true,
        description: true,
        project: true,
        status: true,
        priority: true,
        author: true,
        assigned_to: true,
        created_on: true,
        updated_on: true,
    };
    const NONE: Self = Self {
        subject: false,
        description: false,
        project: false,
        status: false,
        priority: false,
        author: false,
        assigned_to: false,
        created_on: false,
        updated_on: false,
    };
}

/// Resolve the `fields` parameter (G5): absent, `["*"]`, or `["all"]` means
/// every field; otherwise only the named ones (`id`/`tracker` are always
/// included regardless and accepted-but-redundant in the list). An unknown
/// name is an **argument** error, not a tool result (D5-adjacent: the model
/// gave us a value it can fix without calling Redmine).
fn resolve_issue_fields(fields: Option<&[String]>) -> Result<IssueFieldSet, McpError> {
    let Some(fields) = fields else {
        return Ok(IssueFieldSet::ALL);
    };
    if fields.iter().any(|f| f == "*" || f == "all") {
        return Ok(IssueFieldSet::ALL);
    }
    let mut set = IssueFieldSet::NONE;
    for f in fields {
        match f.as_str() {
            "id" | "tracker" => {}
            "subject" => set.subject = true,
            "description" => set.description = true,
            "project" => set.project = true,
            "status" => set.status = true,
            "priority" => set.priority = true,
            "author" => set.author = true,
            "assigned_to" => set.assigned_to = true,
            "created_on" => set.created_on = true,
            "updated_on" => set.updated_on = true,
            other => {
                return Err(McpError::invalid_params(
                    format!(
                        "unknown fields entry {other:?}; expected one of id, tracker, {}, or \"*\"/\"all\"",
                        OPTIONAL_ISSUE_FIELD_NAMES.join(", ")
                    ),
                    None,
                ));
            }
        }
    }
    Ok(set)
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IssueSummaryOut {
    pub(crate) id: u64,
    pub(crate) tracker: IdNameOut,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) project: Option<IdNameOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<IdNameOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) priority: Option<IdNameOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) author: Option<IdNameOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) assigned_to: Option<IdNameOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) created_on: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) updated_on: Option<DateTime<Utc>>,
}

fn issue_summary_out(boundary: &Boundary, issue: &Issue, fields: IssueFieldSet) -> IssueSummaryOut {
    IssueSummaryOut {
        id: issue.id,
        tracker: id_name_out(boundary, "tracker.name", &issue.tracker),
        subject: fields
            .subject
            .then(|| boundary.wrap("issue.subject", &issue.subject)),
        description: fields.description.then_some(()).and_then(|()| {
            issue
                .description
                .as_deref()
                .map(|d| boundary.wrap("issue.description", d))
        }),
        project: fields
            .project
            .then(|| id_name_out(boundary, "project.name", &issue.project)),
        status: fields
            .status
            .then(|| id_name_out(boundary, "issue_status.name", &issue.status)),
        priority: fields
            .priority
            .then(|| id_name_out(boundary, "issue_priority.name", &issue.priority)),
        author: fields
            .author
            .then(|| id_name_out(boundary, "user.name", &issue.author)),
        assigned_to: fields.assigned_to.then_some(()).and_then(|()| {
            issue
                .assigned_to
                .as_ref()
                .map(|a| id_name_out(boundary, "user.name", a))
        }),
        created_on: fields.created_on.then_some(issue.created_on),
        updated_on: fields.updated_on.then_some(issue.updated_on),
    }
}

const ISSUES_MIN_LIMIT: u32 = 1;
const ISSUES_MAX_LIMIT: u32 = 1000;
const ISSUES_DEFAULT_LIMIT: u32 = 25;

/// Clamp to [1, 1000], matching the reference contract for both
/// `list_redmine_issues` and `search_redmine_issues`.
fn clamp_issues_limit(limit: Option<u32>) -> u32 {
    limit
        .unwrap_or(ISSUES_DEFAULT_LIMIT)
        .clamp(ISSUES_MIN_LIMIT, ISSUES_MAX_LIMIT)
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IssuesOutput {
    pub(crate) issues: Vec<IssueSummaryOut>,
    /// Present only when `include_pagination_info=true` was passed — an
    /// absent key, not a `null` value (D2's rationale: the field's very
    /// presence is the caller-visible signal, matching the reference).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pagination: Option<Pagination>,
}

// --- get_redmine_issue ---

const fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the reference contract's six independent include_* flags exactly"
)]
pub(crate) struct GetRedmineIssueParams {
    /// The id of the issue to retrieve.
    pub(crate) issue_id: u64,
    /// Include journals (comments and field-change history). Default true.
    #[serde(default = "default_true")]
    pub(crate) include_journals: bool,
    /// Include attachment metadata. Default true.
    #[serde(default = "default_true")]
    pub(crate) include_attachments: bool,
    /// Include custom field values. Default true.
    #[serde(default = "default_true")]
    pub(crate) include_custom_fields: bool,
    /// Maximum number of journals to return, applied client-side after
    /// fetching every visible journal (Redmine has no server-side journal
    /// pagination). When set, enables `journal_pagination` in the result and
    /// implies journals are fetched even if `include_journals=false`.
    #[serde(default)]
    pub(crate) journal_limit: Option<u32>,
    /// Number of journals to skip, used with `journal_limit`. Default 0.
    #[serde(default)]
    pub(crate) journal_offset: Option<u64>,
    /// Include the watcher list. Default false.
    #[serde(default)]
    pub(crate) include_watchers: bool,
    /// Include issue relations. Default false.
    #[serde(default)]
    pub(crate) include_relations: bool,
    /// Include direct sub-issues, nested one level deep. Default false. Use
    /// `list_subtasks` to walk a deeper tree.
    #[serde(default)]
    pub(crate) include_children: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct JournalOut {
    pub(crate) id: u64,
    pub(crate) user: Option<IdNameOut>,
    pub(crate) notes: Option<String>,
    pub(crate) created_on: DateTime<Utc>,
    pub(crate) private_notes: Option<bool>,
}

fn journal_out(boundary: &Boundary, j: &ClientJournal) -> JournalOut {
    JournalOut {
        id: j.id,
        user: j
            .user
            .as_ref()
            .map(|u| id_name_out(boundary, "user.name", u)),
        notes: j
            .notes
            .as_deref()
            .map(|n| boundary.wrap("journal.notes", n)),
        created_on: j.created_on,
        private_notes: j.private_notes,
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct JournalPagination {
    pub(crate) total: u64,
    pub(crate) offset: u64,
    pub(crate) limit: u32,
    pub(crate) count: u64,
    pub(crate) has_more: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct AttachmentOut {
    pub(crate) id: u64,
    pub(crate) filename: String,
    pub(crate) filesize: u64,
    pub(crate) content_type: Option<String>,
    pub(crate) description: Option<String>,
    /// Passed through verbatim — a mechanical download URL, not free text.
    pub(crate) content_url: String,
    pub(crate) author: Option<IdNameOut>,
    pub(crate) created_on: DateTime<Utc>,
}

fn attachment_out(boundary: &Boundary, a: &Attachment) -> AttachmentOut {
    AttachmentOut {
        id: a.id,
        filename: boundary.wrap("attachment.filename", &a.filename),
        filesize: a.filesize,
        content_type: a.content_type.clone(),
        description: a
            .description
            .as_deref()
            .map(|d| boundary.wrap("attachment.description", d)),
        content_url: a.content_url.clone(),
        author: a
            .author
            .as_ref()
            .map(|u| id_name_out(boundary, "user.name", u)),
        created_on: a.created_on,
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct RelationOut {
    pub(crate) id: u64,
    pub(crate) issue_id: u64,
    pub(crate) issue_to_id: u64,
    pub(crate) relation_type: String,
    pub(crate) delay: Option<i64>,
}

fn relation_out(r: &ClientIssueRelation) -> RelationOut {
    RelationOut {
        id: r.id,
        issue_id: r.issue_id,
        issue_to_id: r.issue_to_id,
        relation_type: r.relation_type.clone(),
        delay: r.delay,
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IssueChildLeafOut {
    pub(crate) id: u64,
    pub(crate) tracker: Option<IdNameOut>,
    pub(crate) subject: String,
}

fn issue_child_leaf_out(boundary: &Boundary, c: &ClientIssueChildLeaf) -> IssueChildLeafOut {
    IssueChildLeafOut {
        id: c.id,
        tracker: c
            .tracker
            .as_ref()
            .map(|t| id_name_out(boundary, "tracker.name", t)),
        subject: boundary.wrap("issue.subject", &c.subject),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IssueChildOut {
    pub(crate) id: u64,
    pub(crate) tracker: Option<IdNameOut>,
    pub(crate) subject: String,
    pub(crate) children: Option<Vec<IssueChildLeafOut>>,
}

fn issue_child_out(boundary: &Boundary, c: &ClientIssueChild) -> IssueChildOut {
    IssueChildOut {
        id: c.id,
        tracker: c
            .tracker
            .as_ref()
            .map(|t| id_name_out(boundary, "tracker.name", t)),
        subject: boundary.wrap("issue.subject", &c.subject),
        children: c.children.as_ref().map(|cs| {
            cs.iter()
                .map(|g| issue_child_leaf_out(boundary, g))
                .collect()
        }),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct CustomFieldValueOut {
    pub(crate) id: u64,
    pub(crate) name: String,
    /// A string, an array of strings (`multiple = true` fields), or `null`.
    pub(crate) value: Option<serde_json::Value>,
}

fn custom_field_value_out(boundary: &Boundary, cf: &CustomField) -> CustomFieldValueOut {
    let value = match &cf.value {
        None | Some(CustomFieldValue::Single(None)) => None,
        Some(CustomFieldValue::Single(Some(s))) => Some(serde_json::Value::String(
            boundary.wrap("issue.custom_field.value", s),
        )),
        Some(CustomFieldValue::Multiple(items)) => Some(serde_json::Value::Array(
            items
                .iter()
                .map(|s| serde_json::Value::String(boundary.wrap("issue.custom_field.value", s)))
                .collect(),
        )),
    };
    CustomFieldValueOut {
        id: cf.id,
        name: boundary.wrap("issue.custom_field.name", &cf.name),
        value,
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IssueDetailOutput {
    pub(crate) id: u64,
    pub(crate) project: IdNameOut,
    pub(crate) tracker: IdNameOut,
    pub(crate) status: IdNameOut,
    pub(crate) priority: IdNameOut,
    pub(crate) author: IdNameOut,
    pub(crate) assigned_to: Option<IdNameOut>,
    pub(crate) parent: Option<IdOnlyOut>,
    pub(crate) category: Option<IdNameOut>,
    pub(crate) fixed_version: Option<IdNameOut>,
    pub(crate) subject: String,
    pub(crate) description: Option<String>,
    pub(crate) start_date: Option<NaiveDate>,
    pub(crate) due_date: Option<NaiveDate>,
    pub(crate) done_ratio: Option<u8>,
    pub(crate) is_private: Option<bool>,
    pub(crate) estimated_hours: Option<f64>,
    /// `None` is ambiguous between "zero hours logged" and "not visible to
    /// this credential" — Redmine itself makes no distinction (see
    /// `redmine-client`'s `Issue::spent_hours` doc comment).
    pub(crate) spent_hours: Option<f64>,
    pub(crate) custom_fields: Option<Vec<CustomFieldValueOut>>,
    pub(crate) created_on: DateTime<Utc>,
    pub(crate) updated_on: DateTime<Utc>,
    pub(crate) closed_on: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) journals: Option<Vec<JournalOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) journal_pagination: Option<JournalPagination>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) attachments: Option<Vec<AttachmentOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) watchers: Option<Vec<IdNameOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) relations: Option<Vec<RelationOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) children: Option<Vec<IssueChildOut>>,
}

// --- list_redmine_issues ---

/// D5: `assigned_to_id` is an integer user id or the literal string `"me"`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub(crate) enum AssignedToRef {
    /// A specific user id, e.g. `5`.
    Id(u64),
    /// The literal string `"me"` — resolves to the caller's own user id.
    Literal(String),
}

fn resolve_assigned_to(r: AssignedToRef) -> Result<UserFilter, McpError> {
    match r {
        AssignedToRef::Id(id) => Ok(UserFilter::Id(UserId(id))),
        AssignedToRef::Literal(s) if s == "me" => Ok(UserFilter::Me),
        AssignedToRef::Literal(other) => Err(McpError::invalid_params(
            format!("assigned_to_id must be an integer or the literal \"me\", got {other:?}"),
            None,
        )),
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListRedmineIssuesParams {
    /// Restrict to one project: numeric id or slug identifier.
    #[serde(default)]
    pub(crate) project_id: Option<ProjectRef>,
    /// Filter by status id. Absent means Redmine's own default (open
    /// issues only) — pass an explicit id to see closed issues.
    #[serde(default)]
    pub(crate) status_id: Option<u64>,
    /// Filter by tracker id.
    #[serde(default)]
    pub(crate) tracker_id: Option<u64>,
    /// Filter by assignee: a numeric user id, or `"me"` for the credential's
    /// own user.
    #[serde(default)]
    pub(crate) assigned_to_id: Option<AssignedToRef>,
    /// Filter by priority id.
    #[serde(default)]
    pub(crate) priority_id: Option<u64>,
    /// Filter by target version (roadmap milestone) id.
    #[serde(default)]
    pub(crate) fixed_version_id: Option<u64>,
    /// Redmine sort syntax, e.g. `"updated_on:desc"`.
    #[serde(default)]
    pub(crate) sort: Option<String>,
    /// Page size, clamped to 1-1000. Defaults to 25.
    #[serde(default)]
    pub(crate) limit: Option<u32>,
    /// Offset of the first result. Defaults to 0.
    #[serde(default)]
    pub(crate) offset: Option<u64>,
    /// Include the `pagination` member in the result. Default false.
    #[serde(default)]
    pub(crate) include_pagination_info: bool,
    /// Restrict which fields each issue carries. Omit for every field;
    /// `["*"]`/`["all"]` also means every field. `id` and `tracker` are
    /// always present regardless.
    #[serde(default)]
    pub(crate) fields: Option<Vec<String>>,
}

// --- search_redmine_issues ---

/// The reference contract's documented (singular) scope values. Translated
/// to Redmine's real wire values inside `redmine_client::model::search`
/// (G1) — `MyProject` becomes `scope=my_projects`, not the literal
/// `my_project`.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SearchScopeParam {
    All,
    MyProject,
    Subprojects,
}

impl From<SearchScopeParam> for SearchScope {
    fn from(p: SearchScopeParam) -> Self {
        match p {
            SearchScopeParam::All => Self::All,
            SearchScopeParam::MyProject => Self::MyProject,
            SearchScopeParam::Subprojects => Self::Subprojects,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchRedmineIssuesParams {
    /// The search text. Must not be empty.
    pub(crate) query: String,
    /// Page size, clamped to 1-1000. Defaults to 25.
    #[serde(default)]
    pub(crate) limit: Option<u32>,
    /// Offset of the first result. Defaults to 0.
    #[serde(default)]
    pub(crate) offset: Option<u64>,
    /// Include the `pagination` member in the result. Default false.
    #[serde(default)]
    pub(crate) include_pagination_info: bool,
    /// Restrict which fields each issue carries. See `list_redmine_issues`.
    #[serde(default)]
    pub(crate) fields: Option<Vec<String>>,
    /// Restrict which projects are searched. Default: all.
    #[serde(default)]
    pub(crate) scope: Option<SearchScopeParam>,
    /// Search only open issues. Default false.
    #[serde(default)]
    pub(crate) open_issues: bool,
}

// --- list_subtasks ---

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct IssueIdParams {
    /// The issue id.
    pub(crate) issue_id: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct SubtasksOutput {
    pub(crate) subtasks: Vec<IssueSummaryOut>,
}

// --- get_private_notes ---

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct PrivateNoteOut {
    pub(crate) id: u64,
    pub(crate) user: Option<IdNameOut>,
    pub(crate) notes: String,
    pub(crate) created_on: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct PrivateNotesOutput {
    pub(crate) private_notes: Vec<PrivateNoteOut>,
}

#[tool_router(router = issues_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// `GET /issues/{id}.json?include=...`.
    #[tool(
        description = "Retrieve full details of one Redmine issue by numeric id, including by default journals and attachments. Use this when the issue id is already known; use list_redmine_issues or search_redmine_issues to find one first. include_watchers/include_relations/include_children default false; journal_limit pages long journal history. children nests one level deep — use list_subtasks for deeper trees.",
        input_schema = crate::tools::schema::input::<GetRedmineIssueParams>(),
        output_schema = crate::tools::schema::output::<IssueDetailOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn get_redmine_issue(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<GetRedmineIssueParams>,
    ) -> Result<CallToolResult, McpError> {
        let want_journals = params.include_journals || params.journal_limit.is_some();
        let mut includes = Vec::new();
        if want_journals {
            includes.push(IssueInclude::Journals);
        }
        if params.include_attachments {
            includes.push(IssueInclude::Attachments);
        }
        if params.include_watchers {
            includes.push(IssueInclude::Watchers);
        }
        if params.include_relations {
            includes.push(IssueInclude::Relations);
        }
        if params.include_children {
            includes.push(IssueInclude::Children);
        }

        let scoped = self.scoped(&ctx)?;
        let mut issue = match scoped.get_issue(IssueId(params.issue_id), &includes).await {
            Ok(issue) => issue,
            Err(e) => return Ok(to_tool_error(e)),
        };
        if !params.include_custom_fields {
            issue.custom_fields = None;
        }

        let boundary = Boundary::new();

        let (journals, journal_pagination) = match (issue.journals.take(), params.journal_limit) {
            (Some(all), Some(limit)) => {
                let offset = params.journal_offset.unwrap_or(0);
                let total = u64::try_from(all.len()).unwrap_or(u64::MAX);
                let offset_usize = usize::try_from(offset).unwrap_or(usize::MAX);
                let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);
                let page: Vec<JournalOut> = all
                    .iter()
                    .skip(offset_usize)
                    .take(limit_usize)
                    .map(|j| journal_out(&boundary, j))
                    .collect();
                let count = u64::try_from(page.len()).unwrap_or(u64::MAX);
                let has_more = offset.saturating_add(count) < total;
                (
                    Some(page),
                    Some(JournalPagination {
                        total,
                        offset,
                        limit,
                        count,
                        has_more,
                    }),
                )
            }
            (Some(all), None) => (
                Some(all.iter().map(|j| journal_out(&boundary, j)).collect()),
                None,
            ),
            (None, _) => (None, None),
        };

        let output = IssueDetailOutput {
            id: issue.id,
            project: id_name_out(&boundary, "project.name", &issue.project),
            tracker: id_name_out(&boundary, "tracker.name", &issue.tracker),
            status: id_name_out(&boundary, "issue_status.name", &issue.status),
            priority: id_name_out(&boundary, "issue_priority.name", &issue.priority),
            author: id_name_out(&boundary, "user.name", &issue.author),
            assigned_to: issue
                .assigned_to
                .as_ref()
                .map(|u| id_name_out(&boundary, "user.name", u)),
            parent: issue.parent.as_ref().map(|p| IdOnlyOut { id: p.id }),
            category: issue
                .category
                .as_ref()
                .map(|c| id_name_out(&boundary, "issue_category.name", c)),
            fixed_version: issue
                .fixed_version
                .as_ref()
                .map(|v| id_name_out(&boundary, "version.name", v)),
            subject: boundary.wrap("issue.subject", &issue.subject),
            description: issue
                .description
                .as_deref()
                .map(|d| boundary.wrap("issue.description", d)),
            start_date: issue.start_date,
            due_date: issue.due_date,
            done_ratio: issue.done_ratio,
            is_private: issue.is_private,
            estimated_hours: issue.estimated_hours,
            spent_hours: issue.spent_hours,
            custom_fields: issue.custom_fields.as_ref().map(|fields| {
                fields
                    .iter()
                    .map(|cf| custom_field_value_out(&boundary, cf))
                    .collect()
            }),
            created_on: issue.created_on,
            updated_on: issue.updated_on,
            closed_on: issue.closed_on,
            journals,
            journal_pagination,
            attachments: issue
                .attachments
                .as_ref()
                .map(|atts| atts.iter().map(|a| attachment_out(&boundary, a)).collect()),
            watchers: issue.watchers.as_ref().map(|ws| {
                ws.iter()
                    .map(|w| id_name_out(&boundary, "user.name", w))
                    .collect()
            }),
            relations: issue
                .relations
                .as_ref()
                .map(|rs| rs.iter().map(relation_out).collect()),
            children: issue
                .children
                .as_ref()
                .map(|cs| cs.iter().map(|c| issue_child_out(&boundary, c)).collect()),
        };

        Ok(output::ok(&output, self.output_caps()))
    }

    /// `GET /issues.json`, a single explicit page.
    #[tool(
        description = "List Redmine issues with flexible filtering and pagination. Supports filtering by project, status, tracker, priority, assignee, and target version. Use this for advanced filtering by field value; use search_redmine_issues for free-text search instead. An empty list means nothing matched — try widening the filters before retrying.",
        input_schema = crate::tools::schema::input::<ListRedmineIssuesParams>(),
        output_schema = crate::tools::schema::output::<IssuesOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_redmine_issues(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ListRedmineIssuesParams>,
    ) -> Result<CallToolResult, McpError> {
        let fields = resolve_issue_fields(params.fields.as_deref())?;
        let project = params.project_id.map(resolve_project_ref).transpose()?;
        let assigned_to = params.assigned_to_id.map(resolve_assigned_to).transpose()?;
        let limit = clamp_issues_limit(params.limit);
        let offset = params.offset.unwrap_or(0);

        let mut extra = BTreeMap::new();
        if let Some(tracker_id) = params.tracker_id {
            extra.insert("tracker_id".to_string(), tracker_id.to_string());
        }
        if let Some(priority_id) = params.priority_id {
            extra.insert("priority_id".to_string(), priority_id.to_string());
        }
        if let Some(fixed_version_id) = params.fixed_version_id {
            extra.insert("fixed_version_id".to_string(), fixed_version_id.to_string());
        }
        let query = IssueQuery {
            project,
            status: params.status_id.map(StatusFilter::Id),
            assigned_to,
            updated_on: None,
            sort: params.sort,
            extra,
        };

        let scoped = self.scoped(&ctx)?;
        let page = match scoped.list_issues_page(&query, limit, offset).await {
            Ok(page) => page,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let issues = page
            .items
            .iter()
            .map(|i| issue_summary_out(&boundary, i, fields))
            .collect();
        let pagination = params
            .include_pagination_info
            .then(|| Pagination::from_page(&page));

        Ok(output::ok(
            &IssuesOutput { issues, pagination },
            self.output_caps(),
        ))
    }

    /// `GET /search.json?issues=1`, then hydrated via
    /// `GET /issues.json?issue_id=...&status_id=*` (G3).
    #[tool(
        description = "Search issues by free text, with pagination and native Search API filters (scope, open_issues). Use this for text-based search; use list_redmine_issues for filtering by exact field values (project_id, status_id, priority_id, etc). An empty list means nothing matched the search text.",
        input_schema = crate::tools::schema::input::<SearchRedmineIssuesParams>(),
        output_schema = crate::tools::schema::output::<IssuesOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn search_redmine_issues(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<SearchRedmineIssuesParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.query.trim().is_empty() {
            return Err(McpError::invalid_params("query must not be empty", None));
        }
        let fields = resolve_issue_fields(params.fields.as_deref())?;
        let limit = clamp_issues_limit(params.limit);
        let offset = params.offset.unwrap_or(0);
        let search_query = SearchQuery {
            q: params.query,
            scope: params.scope.map(Into::into),
            open_issues: params.open_issues,
        };

        let scoped = self.scoped(&ctx)?;
        let search_page = match scoped
            .search_issues_page(&search_query, limit, offset)
            .await
        {
            Ok(page) => page,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let ids: Vec<IssueId> = search_page
            .items
            .iter()
            .filter(|r| r.kind == "issue")
            .map(|r| IssueId(r.id))
            .collect();

        let hydrated = if ids.is_empty() {
            Vec::new()
        } else {
            match scoped.list_issues_by_id(&ids).await {
                Ok(issues) => issues,
                Err(e) => return Ok(to_tool_error(e)),
            }
        };
        let by_id: HashMap<u64, &Issue> = hydrated.iter().map(|i| (i.id, i)).collect();

        let boundary = Boundary::new();
        // Restore search-result order: Redmine's `issue_id=` filter does not
        // promise to preserve the order of the ids listed (G3).
        let issues: Vec<IssueSummaryOut> = ids
            .iter()
            .filter_map(|id| by_id.get(&id.0))
            .map(|i| issue_summary_out(&boundary, i, fields))
            .collect();
        let pagination = params
            .include_pagination_info
            .then(|| Pagination::from_page(&search_page));

        Ok(output::ok(
            &IssuesOutput { issues, pagination },
            self.output_caps(),
        ))
    }

    /// `GET /issues.json?parent_id={id}&status_id=*`, auto-paged.
    #[tool(
        description = "List subtasks (child issues) of a given issue, including closed ones. Use this to see the full immediate-child list; get_redmine_issue's own children field nests only one level. An empty list means the issue has no subtasks.",
        input_schema = crate::tools::schema::input::<IssueIdParams>(),
        output_schema = crate::tools::schema::output::<SubtasksOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_subtasks(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<IssueIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let issues = match scoped.list_subtasks(IssueId(params.issue_id)).await {
            Ok(issues) => issues,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let subtasks = issues
            .iter()
            .map(|i| issue_summary_out(&boundary, i, IssueFieldSet::ALL))
            .collect();

        Ok(output::ok(&SubtasksOutput { subtasks }, self.output_caps()))
    }

    /// `GET /issues/{id}.json?include=journals`, filtered to
    /// `private_notes=true` entries with non-empty text.
    #[tool(
        description = "Retrieve only the private notes (journals with private_notes=true and non-empty text) of an issue. Use this instead of get_redmine_issue when only private notes are wanted. An empty list means either no private notes exist, or the credential lacks the \"View private notes\" permission — this tool cannot tell the two apart, so do not assume an empty result means none exist.",
        input_schema = crate::tools::schema::input::<IssueIdParams>(),
        output_schema = crate::tools::schema::output::<PrivateNotesOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn get_private_notes(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<IssueIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let issue = match scoped
            .get_issue(IssueId(params.issue_id), &[IssueInclude::Journals])
            .await
        {
            Ok(issue) => issue,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let private_notes = issue
            .journals
            .unwrap_or_default()
            .into_iter()
            .filter(|j| j.private_notes == Some(true))
            .filter_map(|j| {
                let notes = j.notes.filter(|n| !n.is_empty())?;
                Some(PrivateNoteOut {
                    id: j.id,
                    user: j
                        .user
                        .as_ref()
                        .map(|u| id_name_out(&boundary, "user.name", u)),
                    notes: boundary.wrap("journal.notes", &notes),
                    created_on: j.created_on,
                })
            })
            .collect();

        Ok(output::ok(
            &PrivateNotesOutput { private_notes },
            self.output_caps(),
        ))
    }
}

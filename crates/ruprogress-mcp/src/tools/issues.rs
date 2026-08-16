//! Issue tools. Reads: `get_redmine_issue`, `list_redmine_issues`,
//! `search_redmine_issues`, `list_subtasks`, `get_private_notes`. Writes/
//! mixed: `create_redmine_issue`, `update_redmine_issue`,
//! `delete_redmine_issue`, `copy_issue`, `manage_issue_relation`,
//! `manage_issue_watcher`, `manage_issue_note`, `manage_issue_category`.
//!
//! `JournalOut` deliberately omits `details` (the field-change history
//! attached to a journal): no example in the reference contract renders it,
//! and an unbounded diff of e.g. a `description` change could itself blow
//! past the response-size byte cap. Revisit if a concrete need for it surfaces.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use bytes::Bytes;
use chrono::{DateTime, NaiveDate, Utc};
use redmine_client::model::attachment::Attachment;
use redmine_client::model::custom_field::CustomFieldValue;
use redmine_client::model::issue::{
    Issue, IssueChild as ClientIssueChild, IssueChildLeaf as ClientIssueChildLeaf, IssueCreate,
    IssueInclude, IssueQuery, IssueUpdate, StatusFilter, UserFilter,
};
use redmine_client::model::issue_category::{IssueCategoryCreate, IssueCategoryUpdate};
use redmine_client::model::journal::{Journal as ClientJournal, JournalUpdate};
use redmine_client::model::plugins::agile::{AgileChange, AgileData, AgileDataAttributes};
use redmine_client::model::relation::{IssueRelation as ClientIssueRelation, IssueRelationCreate};
use redmine_client::model::search::{SearchQuery, SearchScope};
use redmine_client::model::time_entry::TimeEntryQuery;
use redmine_client::model::upload::UploadRef;
use redmine_client::model::{CustomField, IdName};
use redmine_client::{
    AttachmentId, IssueCategoryId, IssueId, JournalId, ProjectId, ProjectIdent, RelationId, UserId,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::to_tool_error;
use crate::render::Boundary;
use crate::server::RedmineMcp;
use crate::tools::discovery::{ProjectRef, resolve_project_ref};
use crate::tools::files;
use crate::tools::output::{self, ContentUrlRewrite, ErrorCode, Pagination};

// --- shared output shapes ---

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IdNameOut {
    pub(crate) id: u64,
    pub(crate) name: String,
}

pub(crate) fn id_name_out(boundary: &Boundary, kind: &str, v: &IdName) -> IdNameOut {
    IdNameOut {
        id: v.id,
        name: boundary.wrap(kind, &v.name),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IdOnlyOut {
    pub(crate) id: u64,
}

// --- shared `fields` selection ---

/// Field names the reference contract accepts for `list_redmine_issues` and
/// `search_redmine_issues`'s `fields` parameter, minus `id`/`tracker` (always
/// included, never filterable).
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

/// Resolve the `fields` parameter: absent, `["*"]`, or `["all"]` means
/// every field; otherwise only the named ones (`id`/`tracker` are always
/// included regardless and accepted-but-redundant in the list). An unknown
/// name is an **argument** error, not a tool result: the model
/// gave us a value it can fix without calling Redmine.
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
    /// absent key, not a `null` value: the field's very
    /// presence is the caller-visible signal, matching the reference.
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
    /// Passed through verbatim (modulo the `REDMINE_PUBLIC_URL` rewrite) — a
    /// mechanical download URL, not free text.
    pub(crate) content_url: String,
    pub(crate) author: Option<IdNameOut>,
    pub(crate) created_on: DateTime<Utc>,
}

pub(crate) fn attachment_out(
    boundary: &Boundary,
    rewrite: &ContentUrlRewrite<'_>,
    a: &Attachment,
) -> AttachmentOut {
    AttachmentOut {
        id: a.id,
        filename: boundary.wrap("attachment.filename", &a.filename),
        filesize: a.filesize,
        content_type: a.content_type.clone(),
        description: a
            .description
            .as_deref()
            .map(|d| boundary.wrap("attachment.description", d)),
        content_url: rewrite.apply(&a.content_url),
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
#[allow(
    clippy::option_option,
    reason = "story_points/agile_sprint_id/agile_position need three states \
              (absent/null/value); see the field doc comment"
)]
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
    /// `RedmineUP` Agile plugin fields. Three-way: the key is **absent**
    /// when `REDMINE_AGILE_ENABLED` is off or the agile fetch failed
    /// (logged, never fatal to this tool); it is present and `null` when
    /// the issue genuinely has no agile row; it carries a value when the
    /// issue has one. `update_redmine_issue` only sets these when the call
    /// itself changed an agile field.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<u32>")]
    pub(crate) story_points: Option<Option<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<u64>")]
    pub(crate) agile_sprint_id: Option<Option<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<u32>")]
    pub(crate) agile_position: Option<Option<u32>>,
    /// `AlphaNodes` `additional_tags` plugin tags. Absent both when
    /// `REDMINE_TAGS_ENABLED` is off and when the plugin itself omitted the
    /// key (e.g. the caller lacks `view_issue_tags`) — the two cases are
    /// indistinguishable on the wire, so this field can only promise
    /// "nothing to report", never "no tags".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tags: Option<Vec<TagOut>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct TagOut {
    /// The plugin's internal tag id. Not durable — `name` is the
    /// identifier; `id` is frequently absent on the wire.
    pub(crate) id: Option<u64>,
    pub(crate) name: String,
}

fn tag_out(boundary: &Boundary, t: &redmine_client::model::plugins::tags::IssueTag) -> TagOut {
    TagOut {
        id: t.id,
        name: boundary.wrap("issue.tag.name", &t.name),
    }
}

// --- list_redmine_issues ---

/// `assigned_to_id` is an integer user id or the literal string `"me"`.
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
/// — `MyProject` becomes `scope=my_projects`, not the literal
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

// --- create_redmine_issue / update_redmine_issue ---

/// One file to attach as part of `create_redmine_issue`/`update_redmine_issue`.
/// Same source rules as `upload_file`'s own parameters
/// (`tools/files.rs::UploadFileParams`): exactly one of `content_base64`/
/// `file_path`/`source_url` — the latter always refused with
/// `UNSUPPORTED_SOURCE`, deferred to a future release.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct IssueUploadParams {
    /// Name the attachment will have in Redmine. Required when using
    /// `content_base64`; inferred from the path when using `file_path`.
    #[serde(default)]
    pub(crate) filename: Option<String>,
    /// Raw file bytes, base64-encoded. Exactly one of `content_base64`/
    /// `file_path` must be set.
    #[serde(default)]
    pub(crate) content_base64: Option<String>,
    /// Absolute path to a file already on this server: inside
    /// `ATTACHMENTS_DIR` or a directory listed in
    /// `REDMINE_MCP_UPLOAD_FILE_ROOTS`. Limited to 50 MiB.
    #[serde(default)]
    pub(crate) file_path: Option<String>,
    /// Not supported by this server. Present only so a caller who sends it
    /// gets a precise `UNSUPPORTED_SOURCE` refusal instead of a schema
    /// error; use `content_base64` or `file_path` instead.
    #[serde(default)]
    pub(crate) source_url: Option<String>,
    /// Human-readable description shown on the attachment.
    #[serde(default)]
    pub(crate) description: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateRedmineIssueParams {
    /// The project to create the issue in: numeric id or slug identifier.
    pub(crate) project_id: ProjectRef,
    /// The issue subject/title. Must not be empty.
    pub(crate) subject: String,
    /// The issue description.
    #[serde(default)]
    pub(crate) description: Option<String>,
    /// The tracker id, if not the project's default.
    #[serde(default)]
    pub(crate) tracker_id: Option<u64>,
    /// The status id, if not the tracker's default.
    #[serde(default)]
    pub(crate) status_id: Option<u64>,
    /// The priority id, if not the default.
    #[serde(default)]
    pub(crate) priority_id: Option<u64>,
    /// Who to assign the issue to.
    #[serde(default)]
    pub(crate) assigned_to_id: Option<u64>,
    /// The issue category id.
    #[serde(default)]
    pub(crate) category_id: Option<u64>,
    /// The target version (roadmap milestone) id.
    #[serde(default)]
    pub(crate) fixed_version_id: Option<u64>,
    /// Parent issue id, to create this as a sub-issue.
    #[serde(default)]
    pub(crate) parent_issue_id: Option<u64>,
    /// Planned start date (`YYYY-MM-DD`).
    #[serde(default)]
    pub(crate) start_date: Option<NaiveDate>,
    /// Planned due date (`YYYY-MM-DD`).
    #[serde(default)]
    pub(crate) due_date: Option<NaiveDate>,
    /// Percent done, 0-100.
    #[serde(default)]
    pub(crate) done_ratio: Option<u8>,
    /// Estimated hours.
    #[serde(default)]
    pub(crate) estimated_hours: Option<f64>,
    /// Whether the issue is private.
    #[serde(default)]
    pub(crate) is_private: Option<bool>,
    /// Files to attach to the issue in this same request. Maximum 10 items;
    /// each item follows the same source rules as `upload_file`.
    #[serde(default)]
    pub(crate) uploads: Option<Vec<IssueUploadParams>>,
    /// Tags to set on the new issue (`AlphaNodes` `additional_tags` plugin).
    /// A tag name containing a comma is rejected — pass separate array
    /// entries instead of a comma-separated string. Requires
    /// `REDMINE_TAGS_ENABLED`; using this while the plugin is disabled
    /// returns a `MISCONFIGURED` error before any write happens.
    #[serde(default)]
    pub(crate) tag_list: Option<Vec<String>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct CreateRedmineIssueOutput {
    pub(crate) success: bool,
    pub(crate) issue: IssueDetailOutput,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one bool (is_private) among many independent optional fields; splitting it out would not help readability"
)]
#[allow(
    clippy::option_option,
    reason = "story_points needs three states (absent/null/value); see its field doc comment"
)]
pub(crate) struct UpdateRedmineIssueParams {
    /// The id of the issue to update.
    pub(crate) issue_id: u64,
    /// New subject, if changing it.
    #[serde(default)]
    pub(crate) subject: Option<String>,
    /// New description. An empty string clears it; omit to leave unchanged.
    #[serde(default)]
    pub(crate) description: Option<String>,
    /// New tracker id.
    #[serde(default)]
    pub(crate) tracker_id: Option<u64>,
    /// New status id.
    #[serde(default)]
    pub(crate) status_id: Option<u64>,
    /// New priority id.
    #[serde(default)]
    pub(crate) priority_id: Option<u64>,
    /// New assignee user id.
    #[serde(default)]
    pub(crate) assigned_to_id: Option<u64>,
    /// New category id.
    #[serde(default)]
    pub(crate) category_id: Option<u64>,
    /// New target version id.
    #[serde(default)]
    pub(crate) fixed_version_id: Option<u64>,
    /// New parent issue id, to reparent this issue.
    #[serde(default)]
    pub(crate) parent_issue_id: Option<u64>,
    /// New planned start date.
    #[serde(default)]
    pub(crate) start_date: Option<NaiveDate>,
    /// New planned due date.
    #[serde(default)]
    pub(crate) due_date: Option<NaiveDate>,
    /// New percent done, 0-100.
    #[serde(default)]
    pub(crate) done_ratio: Option<u8>,
    /// New estimated hours.
    #[serde(default)]
    pub(crate) estimated_hours: Option<f64>,
    /// New privacy flag.
    #[serde(default)]
    pub(crate) is_private: Option<bool>,
    /// A note to add to the issue's history, independent of any field
    /// change above. An empty string is rejected as meaningless (use `None`
    /// to add no note).
    #[serde(default)]
    pub(crate) notes: Option<String>,
    /// Whether the note added via `notes` is private. Requires the "set
    /// notes private" permission; ignored if `notes` is not given.
    #[serde(default)]
    pub(crate) private_notes: Option<bool>,
    /// Files to attach to the issue in this same request. Maximum 10 items;
    /// each item follows the same source rules as `upload_file`.
    #[serde(default)]
    pub(crate) uploads: Option<Vec<IssueUploadParams>>,
    /// New story points (`RedmineUP` Agile plugin). Omit to leave unchanged,
    /// `null` to clear, a number to set. Requires `REDMINE_AGILE_ENABLED`;
    /// using this while the plugin is disabled returns a `MISCONFIGURED`
    /// error before any write happens.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[schemars(with = "Option<u32>")]
    pub(crate) story_points: Option<Option<u32>>,
    /// New sprint id (`RedmineUP` Agile plugin). `0` removes the issue from
    /// its sprint — the plugin's own sentinel. Requires
    /// `REDMINE_AGILE_ENABLED`.
    #[serde(default)]
    pub(crate) agile_sprint_id: Option<u64>,
    /// New position within its sprint/board (`RedmineUP` Agile plugin).
    /// Requires `REDMINE_AGILE_ENABLED`.
    #[serde(default)]
    pub(crate) agile_position: Option<u32>,
    /// Replaces the issue's full tag set (`AlphaNodes` `additional_tags`
    /// plugin) — not additive. `[]` clears all tags; omit to leave the tag
    /// set unchanged. A tag name containing a comma is rejected — pass
    /// separate array entries instead of a comma-separated string.
    /// Requires `REDMINE_TAGS_ENABLED`; using this while the plugin is
    /// disabled returns a `MISCONFIGURED` error before any write happens.
    #[serde(default)]
    pub(crate) tag_list: Option<Vec<String>>,
}

/// Validates and trims a `tag_list` parameter (T1): a name that is empty
/// after trimming, or contains a comma, is rejected before any request is
/// sent. Duplicate names pass through unchanged — deduplication is
/// Redmine's job, and silently dropping one would be a data change.
fn validate_tag_list(tags: Vec<String>) -> Result<Vec<String>, McpError> {
    tags.into_iter()
        .map(|tag| {
            let trimmed = tag.trim();
            if trimmed.is_empty() {
                return Err(McpError::invalid_params(
                    format!("tag_list entry {tag:?} is empty after trimming"),
                    None,
                ));
            }
            if trimmed.contains(',') {
                return Err(McpError::invalid_params(
                    format!(
                        "tag_list entry {trimmed:?} contains a comma; pass separate array \
                         entries instead of a comma-separated string"
                    ),
                    None,
                ));
            }
            Ok(trimmed.to_string())
        })
        .collect()
}

/// Distinguishes an absent `story_points` key (`None`, leave unchanged) from
/// a present `null` (`Some(None)`, clear) from a present value
/// (`Some(Some(n))`, set) — the standard serde double-`Option` idiom.
/// Combined with `#[serde(default)]` on the field: `default` alone would
/// collapse "absent" and "present `null`" to the same `None`.
#[allow(
    clippy::option_option,
    reason = "the whole point of this helper is to produce the three-state Option<Option<T>>"
)]
fn deserialize_double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct UpdateRedmineIssueOutput {
    pub(crate) success: bool,
    pub(crate) issue: IssueDetailOutput,
}

// --- delete_redmine_issue ---

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteRedmineIssueParams {
    /// The id of the issue to delete.
    pub(crate) issue_id: u64,
    /// When `false` (default), the tool refuses and returns an impact
    /// preview instead of deleting anything. Pass `true` to actually
    /// delete.
    #[serde(default)]
    pub(crate) confirm_delete: bool,
    /// When the issue has direct subtasks, `confirm_delete=true` alone still
    /// refuses with `code: "CHILDREN_PRESENT"`. Pass this too to opt in:
    /// Redmine cascade-deletes subtasks automatically, with no way to keep
    /// them.
    #[serde(default)]
    pub(crate) confirm_delete_with_children: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
#[allow(
    clippy::struct_field_names,
    reason = "the shared `_count` suffix is the point: this is a preview of what deleting the issue would count away"
)]
pub(crate) struct DeleteImpactOut {
    /// Direct subtasks only (Redmine's `parent_id` filter is not
    /// transitive) — grandchildren are not counted here, but Redmine still
    /// cascade-deletes the whole subtree.
    pub(crate) children_count: u64,
    pub(crate) journals_count: u64,
    pub(crate) attachments_count: u64,
    pub(crate) relations_count: u64,
    pub(crate) time_entries_count: u64,
}

/// A single schema covering both the refusal and success shapes: the
/// reference contract treats a delete refusal as a normal result the model
/// should inspect and react to, not an error (`isError` stays `false`
/// either way).
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct DeleteRedmineIssueOutput {
    pub(crate) success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) code: Option<ErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) impact: Option<DeleteImpactOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) deleted_issue_id: Option<u64>,
    /// Number of subtasks Redmine cascade-deleted along with the issue
    /// (equal to `impact.children_count` from the preview, since Redmine's
    /// destroy always cascades the full subtree once it proceeds at all).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cascade_deleted: Option<u64>,
}

// --- copy_issue ---

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CopyIssueParams {
    /// The id of the source issue to copy.
    pub(crate) issue_id: u64,
    /// Target project for the copy: numeric id or slug identifier. Defaults
    /// to the source issue's project.
    #[serde(default)]
    pub(crate) project_id: Option<ProjectRef>,
    /// New subject for the copy. Defaults to the source subject.
    #[serde(default)]
    pub(crate) subject: Option<String>,
    /// Create a `copied_to`/`copied_from` relation between the original and
    /// the copy. Default true.
    #[serde(default = "default_true")]
    pub(crate) link_original: bool,
    /// Recursively copy the source's direct subtasks (and their own
    /// subtasks, up to a bounded depth/count). Default true.
    #[serde(default = "default_true")]
    pub(crate) copy_subtasks: bool,
    /// Override the assignee on the copy. Defaults to the source's.
    #[serde(default)]
    pub(crate) assigned_to_id: Option<u64>,
    /// Override the tracker on the copy. Defaults to the source's.
    #[serde(default)]
    pub(crate) tracker_id: Option<u64>,
    /// Override the priority on the copy. Defaults to the source's.
    #[serde(default)]
    pub(crate) priority_id: Option<u64>,
    /// Override the category on the copy. Defaults to the source's.
    #[serde(default)]
    pub(crate) category_id: Option<u64>,
    /// Override the target version on the copy. Defaults to the source's.
    #[serde(default)]
    pub(crate) fixed_version_id: Option<u64>,
    /// Override the description on the copy. Defaults to the source's.
    #[serde(default)]
    pub(crate) description: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct CopyIssueOutput {
    pub(crate) success: bool,
    pub(crate) issue: IssueDetailOutput,
    /// Total number of subtasks copied (0 if `copy_subtasks=false` or the
    /// source had none).
    pub(crate) subtasks_copied: u64,
    /// `true` if the subtask copy stopped early because of this server's
    /// bounded copy limit — never a silent cut: what was copied is real and
    /// complete in itself, just not the *whole* source subtree.
    pub(crate) subtasks_truncated: bool,
}

// --- manage_issue_relation ---

const RELATION_TYPES: &[&str] = &[
    "relates",
    "duplicates",
    "duplicated",
    "blocks",
    "blocked",
    "precedes",
    "follows",
    "copied_to",
    "copied_from",
];

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ManageIssueRelationAction {
    List,
    Create,
    Delete,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManageIssueRelationParams {
    /// Operation to perform. `list` is always available, even in read-only
    /// mode; `create`/`delete` are blocked in read-only mode.
    pub(crate) action: ManageIssueRelationAction,
    /// Source issue id. Required for `action="list"` and `action="create"`.
    #[serde(default)]
    pub(crate) issue_id: Option<u64>,
    /// Target issue id. Required for `action="create"`.
    #[serde(default)]
    pub(crate) issue_to_id: Option<u64>,
    /// Relation id. Required for `action="delete"`.
    #[serde(default)]
    pub(crate) relation_id: Option<u64>,
    /// One of `relates`, `duplicates`, `duplicated`, `blocks`, `blocked`,
    /// `precedes`, `follows`, `copied_to`, `copied_from`. Defaults to
    /// `relates` on create.
    #[serde(default)]
    pub(crate) relation_type: Option<String>,
    /// Delay in days. Only meaningful for `precedes`.
    #[serde(default)]
    pub(crate) delay: Option<i64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ManageIssueRelationOutput {
    pub(crate) success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) relation: Option<RelationOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) relations: Option<Vec<RelationOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) deleted_relation_id: Option<u64>,
}

// --- manage_issue_watcher ---

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ManageIssueWatcherAction {
    Add,
    Remove,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManageIssueWatcherParams {
    pub(crate) action: ManageIssueWatcherAction,
    /// The issue id.
    pub(crate) issue_id: u64,
    /// The user id to add or remove as a watcher.
    pub(crate) user_id: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ManageIssueWatcherOutput {
    pub(crate) success: bool,
    pub(crate) issue_id: u64,
    pub(crate) user_id: u64,
}

// --- manage_issue_note ---

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManageIssueNoteAction {
    Edit,
    SetPrivate,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManageIssueNoteParams {
    pub(crate) action: ManageIssueNoteAction,
    /// The journal (note) id, from `get_redmine_issue` with
    /// `include_journals=true` or `get_private_notes`.
    pub(crate) journal_id: u64,
    /// New note text. May be empty to clear it. Required for
    /// `action="edit"`.
    #[serde(default)]
    pub(crate) notes: Option<String>,
    /// Toggle the private flag while editing. Optional for
    /// `action="edit"`.
    #[serde(default)]
    pub(crate) private_notes: Option<bool>,
    /// `true` to mark the note private, `false` to make it public. Required
    /// for `action="set_private"`.
    #[serde(default)]
    pub(crate) is_private: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ManageIssueNoteOutput {
    pub(crate) success: bool,
    pub(crate) journal_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) private_notes: Option<bool>,
}

// --- manage_issue_category ---

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ManageIssueCategoryAction {
    List,
    Create,
    Update,
    Delete,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManageIssueCategoryParams {
    /// Operation to perform. `list` is always available, even in read-only
    /// mode; `create`/`update`/`delete` are blocked in read-only mode.
    pub(crate) action: ManageIssueCategoryAction,
    /// Project identifier. Required for `action="list"` and
    /// `action="create"`.
    #[serde(default)]
    pub(crate) project_id: Option<ProjectRef>,
    /// Category id. Required for `action="update"` and `action="delete"`.
    #[serde(default)]
    pub(crate) category_id: Option<u64>,
    /// Category name. Required for `action="create"`; optional (but not
    /// blank if given) for `action="update"`.
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// Default assignee user id. For `create`/`update`.
    #[serde(default)]
    pub(crate) assigned_to_id: Option<u64>,
    /// Reassign the deleted category's issues to this category id instead
    /// of leaving them uncategorised. For `delete` only.
    #[serde(default)]
    pub(crate) reassign_to_id: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct IssueCategoryOut {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) project: Option<IdNameOut>,
    pub(crate) assigned_to: Option<IdNameOut>,
}

fn issue_category_out(
    boundary: &Boundary,
    c: &redmine_client::model::issue_category::IssueCategory,
) -> IssueCategoryOut {
    IssueCategoryOut {
        id: c.id,
        name: boundary.wrap("issue_category.name", &c.name),
        project: c
            .project
            .as_ref()
            .map(|p| id_name_out(boundary, "project.name", p)),
        assigned_to: c
            .assigned_to
            .as_ref()
            .map(|u| id_name_out(boundary, "user.name", u)),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ManageIssueCategoryOutput {
    pub(crate) success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) category: Option<IssueCategoryOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) categories: Option<Vec<IssueCategoryOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) deleted_category_id: Option<u64>,
}

/// Build an [`IssueDetailOutput`] from a fetched [`Issue`], applying journal
/// pagination when `journal_limit` is given. Shared by
/// `get_redmine_issue`, `create_redmine_issue`, and `update_redmine_issue`
/// (the latter two always pass `journal_limit: None` — Redmine's
/// create/update responses never include journals in the first place, so
/// `issue.journals` is `None` and the match's last arm applies).
///
/// `agile`: `None` unless the caller fetched agile data (`create_redmine_issue`
/// never does; `get_redmine_issue` does whenever the plugin flag is on;
/// `update_redmine_issue` does only when the call changed an agile field).
/// `Some(AgileData::default())` represents "fetched, but the issue has no
/// row" — the fields render as present-and-`null`, not absent.
///
/// `tags_enabled`: whether `REDMINE_TAGS_ENABLED` is on. `issue.tags` came
/// along for free on the same fetch that produced `issue` — unlike agile,
/// there is no separate request to gate — but the flag, not a permissive
/// Redmine sending the key anyway, is the contract: with the flag off the
/// `tags` output key stays absent regardless of what `issue.tags` holds.
fn issue_detail_out(
    boundary: &Boundary,
    rewrite: &ContentUrlRewrite<'_>,
    mut issue: Issue,
    journal_limit: Option<u32>,
    journal_offset: Option<u64>,
    agile: Option<&AgileData>,
    tags_enabled: bool,
) -> IssueDetailOutput {
    let (journals, journal_pagination) = match (issue.journals.take(), journal_limit) {
        (Some(all), Some(limit)) => {
            let offset = journal_offset.unwrap_or(0);
            let total = u64::try_from(all.len()).unwrap_or(u64::MAX);
            let offset_usize = usize::try_from(offset).unwrap_or(usize::MAX);
            let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);
            let page: Vec<JournalOut> = all
                .iter()
                .skip(offset_usize)
                .take(limit_usize)
                .map(|j| journal_out(boundary, j))
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
            Some(all.iter().map(|j| journal_out(boundary, j)).collect()),
            None,
        ),
        (None, _) => (None, None),
    };

    IssueDetailOutput {
        id: issue.id,
        project: id_name_out(boundary, "project.name", &issue.project),
        tracker: id_name_out(boundary, "tracker.name", &issue.tracker),
        status: id_name_out(boundary, "issue_status.name", &issue.status),
        priority: id_name_out(boundary, "issue_priority.name", &issue.priority),
        author: id_name_out(boundary, "user.name", &issue.author),
        assigned_to: issue
            .assigned_to
            .as_ref()
            .map(|u| id_name_out(boundary, "user.name", u)),
        parent: issue.parent.as_ref().map(|p| IdOnlyOut { id: p.id }),
        category: issue
            .category
            .as_ref()
            .map(|c| id_name_out(boundary, "issue_category.name", c)),
        fixed_version: issue
            .fixed_version
            .as_ref()
            .map(|v| id_name_out(boundary, "version.name", v)),
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
                .map(|cf| custom_field_value_out(boundary, cf))
                .collect()
        }),
        created_on: issue.created_on,
        updated_on: issue.updated_on,
        closed_on: issue.closed_on,
        journals,
        journal_pagination,
        attachments: issue.attachments.as_ref().map(|atts| {
            atts.iter()
                .map(|a| attachment_out(boundary, rewrite, a))
                .collect()
        }),
        watchers: issue.watchers.as_ref().map(|ws| {
            ws.iter()
                .map(|w| id_name_out(boundary, "user.name", w))
                .collect()
        }),
        relations: issue
            .relations
            .as_ref()
            .map(|rs| rs.iter().map(relation_out).collect()),
        children: issue
            .children
            .as_ref()
            .map(|cs| cs.iter().map(|c| issue_child_out(boundary, c)).collect()),
        story_points: agile.map(|a| a.story_points),
        agile_sprint_id: agile.map(|a| a.agile_sprint_id),
        agile_position: agile.map(|a| a.position),
        tags: tags_enabled
            .then(|| issue.tags.take())
            .flatten()
            .map(|tags| tags.iter().map(|t| tag_out(boundary, t)).collect()),
    }
}

/// An agile-endpoint failure inside `update_redmine_issue`. When
/// `core_already_applied` is `true`, the issue's non-agile fields already
/// changed successfully in an earlier, separate `PUT` — the message must say
/// so, or the model may retry the whole call and double-apply a note.
fn agile_failure_result(e: redmine_client::Error, core_already_applied: bool) -> CallToolResult {
    let mut result = to_tool_error(e);
    if core_already_applied
        && let Some(structured) = result.structured_content.as_mut()
        && let Some(message) = structured.get("error").and_then(|v| v.as_str())
    {
        let combined = format!(
            "the issue's core fields were already updated successfully; only the agile \
             fields failed to apply: {message}"
        );
        structured["error"] = serde_json::Value::String(combined);
        result.content = vec![ContentBlock::text(structured.to_string())];
    }
    result
}

/// A per-item `uploads[]` failure: either an argument-shape
/// mistake the model must fix before the call means anything (`Protocol`,
/// matching the same "filename required for `content_base64`" precedent as
/// `upload_file`), or a condition that depends on server-side state — path
/// validation, source arity — reported in-band (`InBand`), reusing
/// `tools::files`'s exact
/// `SOURCE_REQUIRED`/`UNSUPPORTED_SOURCE`/`PATH_NOT_ALLOWED`/`FILE_TOO_LARGE`
/// helpers.
enum IssueUploadOutcome {
    Protocol(McpError),
    InBand(CallToolResult),
}

/// Resolves one `uploads[]` item to its raw bytes and effective filename,
/// touching no network (the first of two passes): a validation failure here
/// — on any item — means zero `POST /uploads.json` requests are ever sent
/// for this call, so a bad item never leaves earlier ones half-uploaded to
/// Redmine.
async fn resolve_issue_upload(
    roots: &[PathBuf],
    store_dir: &Path,
    idx: usize,
    item: IssueUploadParams,
) -> Result<(Bytes, Option<String>, Option<String>), IssueUploadOutcome> {
    let IssueUploadParams {
        mut filename,
        content_base64,
        file_path,
        source_url,
        description,
    } = item;

    let sources_set = [
        content_base64.is_some(),
        file_path.is_some(),
        source_url.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if sources_set != 1 {
        return Err(IssueUploadOutcome::InBand(files::source_required(
            &format!("uploads[{idx}]"),
        )));
    }
    if source_url.is_some() {
        return Err(IssueUploadOutcome::InBand(files::unsupported_source()));
    }

    let bytes = if let Some(b64) = content_base64 {
        if filename.is_none() {
            return Err(IssueUploadOutcome::Protocol(McpError::invalid_params(
                format!("uploads[{idx}]: filename is required when using content_base64"),
                None,
            )));
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .map_err(|e| {
                IssueUploadOutcome::Protocol(McpError::invalid_params(
                    format!("uploads[{idx}]: content_base64 is not valid base64: {e}"),
                    None,
                ))
            })?;
        Bytes::from(decoded)
    } else {
        // `sources_set == 1` and `source_url`/`content_base64` are both
        // excluded above, so `file_path` must be set.
        let raw_path = file_path.unwrap_or_default();
        let (contents, inferred) =
            files::read_and_validate_upload_path(roots, store_dir, &raw_path)
                .await
                .map_err(IssueUploadOutcome::InBand)?;
        if filename.is_none() {
            filename = inferred;
        }
        contents
    };

    Ok((bytes, filename, description))
}

/// Two passes for `uploads[]`: resolve every item locally first (no
/// network — a validation failure on item N means zero `POST /uploads.json`
/// requests were sent for *any* item), then mint one upload token per
/// resolved item, sequentially. Returns the `UploadRef`s to embed in the
/// create/update payload and the minted attachment ids, in the same order,
/// for the post-success metadata refetch. `uploads: None` and
/// `uploads: Some(vec![])` both short-circuit to empty results with no
/// network calls at all.
async fn resolve_and_mint_issue_uploads(
    scoped: &redmine_client::Scoped<'_>,
    roots: &[PathBuf],
    store_dir: &Path,
    uploads: Option<Vec<IssueUploadParams>>,
) -> Result<(Vec<UploadRef>, Vec<u64>), IssueUploadOutcome> {
    let Some(uploads) = uploads else {
        return Ok((Vec::new(), Vec::new()));
    };
    if uploads.len() > 10 {
        return Err(IssueUploadOutcome::Protocol(McpError::invalid_params(
            "uploads accepts at most 10 items",
            None,
        )));
    }

    let mut resolved = Vec::with_capacity(uploads.len());
    for (idx, item) in uploads.into_iter().enumerate() {
        resolved.push(resolve_issue_upload(roots, store_dir, idx, item).await?);
    }

    let mut upload_refs = Vec::with_capacity(resolved.len());
    let mut attachment_ids = Vec::with_capacity(resolved.len());
    for (bytes, filename, description) in resolved {
        let upload = files::mint_upload_token(scoped, bytes, filename.as_deref())
            .await
            .map_err(IssueUploadOutcome::InBand)?;
        attachment_ids.push(upload.id);
        upload_refs.push(UploadRef {
            token: upload.token,
            description,
        });
    }
    Ok((upload_refs, attachment_ids))
}

/// Fetches full metadata for a batch of just-minted upload ids — each
/// `Upload::id` from `resolve_and_mint_issue_uploads` is already the
/// resulting attachment's id, so this is a plain `GET` per id, no search
/// needed.
async fn fetch_attachments(
    scoped: &redmine_client::Scoped<'_>,
    ids: &[u64],
) -> redmine_client::Result<Vec<Attachment>> {
    let mut attachments = Vec::with_capacity(ids.len());
    for &id in ids {
        attachments.push(scoped.get_attachment(AttachmentId(id)).await?);
    }
    Ok(attachments)
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

        // Unconditional whenever the plugin flag is on: no `include_agile`
        // parameter, matching the reference. A failed fetch never fails the
        // whole tool — it just omits the three fields.
        let agile = if self.inner.config.plugins.agile {
            match scoped.get_agile_data(IssueId(params.issue_id)).await {
                Ok(row) => Some(row.unwrap_or_default()),
                Err(error) => {
                    tracing::warn!(
                        issue_id = params.issue_id,
                        %error,
                        "get_redmine_issue: agile data fetch failed; omitting agile fields"
                    );
                    None
                }
            }
        } else {
            None
        };

        let boundary = Boundary::new();
        let rewrite = self.content_url_rewrite();
        let output = issue_detail_out(
            &boundary,
            &rewrite,
            issue,
            params.journal_limit,
            params.journal_offset,
            agile.as_ref(),
            self.inner.config.plugins.tags,
        );
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
    /// `GET /issues.json?issue_id=...&status_id=*`.
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
        // promise to preserve the order of the ids listed.
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

    /// `POST /issues.json`.
    #[tool(
        description = "Create a new Redmine issue. Use this to add a task, bug, or feature request to a project. Only project_id and subject are required; every other field defaults to the project's/tracker's own default when omitted. Write tool; blocked in read-only mode.",
        input_schema = crate::tools::schema::input::<CreateRedmineIssueParams>(),
        output_schema = crate::tools::schema::output::<CreateRedmineIssueOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn create_redmine_issue(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreateRedmineIssueParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.subject.trim().is_empty() {
            return Err(McpError::invalid_params("subject must not be empty", None));
        }
        if params.tag_list.is_some() && !self.inner.config.plugins.tags {
            return Ok(output::err(
                ErrorCode::Misconfigured,
                "tag_list requires the AlphaNodes additional_tags plugin",
                Some("set REDMINE_TAGS_ENABLED=true, or omit tag_list"),
            ));
        }
        let tag_list = params.tag_list.map(validate_tag_list).transpose()?;
        let project_id = resolve_project_ref(params.project_id)?;
        let scoped = self.scoped(&ctx)?;

        let store = self.attachments();
        let (uploads, attachment_ids) = match resolve_and_mint_issue_uploads(
            &scoped,
            &self.inner.config.attachments.upload_file_roots,
            store.dir(),
            params.uploads,
        )
        .await
        {
            Ok(v) => v,
            Err(IssueUploadOutcome::Protocol(e)) => return Err(e),
            Err(IssueUploadOutcome::InBand(r)) => return Ok(r),
        };

        let create = IssueCreate {
            project_id,
            subject: params.subject,
            tracker_id: params.tracker_id,
            status_id: params.status_id,
            priority_id: params.priority_id,
            category_id: params.category_id,
            fixed_version_id: params.fixed_version_id,
            description: params.description,
            assigned_to_id: params.assigned_to_id.map(UserId),
            parent_issue_id: params.parent_issue_id.map(IssueId),
            start_date: params.start_date,
            due_date: params.due_date,
            done_ratio: params.done_ratio,
            estimated_hours: params.estimated_hours,
            is_private: params.is_private,
            uploads,
            tag_list,
        };

        let mut issue = match scoped.create_issue(&create).await {
            Ok(issue) => issue,
            Err(e) => return Ok(to_tool_error(e)),
        };
        if !attachment_ids.is_empty() {
            match fetch_attachments(&scoped, &attachment_ids).await {
                Ok(attachments) => issue.attachments = Some(attachments),
                Err(e) => return Ok(to_tool_error(e)),
            }
        }

        let boundary = Boundary::new();
        let rewrite = self.content_url_rewrite();
        // `create_redmine_issue` never touches the agile plugin — see the
        // "Verified endpoint shapes" reasoning above `IssueDetailOutput`.
        let issue_out = issue_detail_out(
            &boundary,
            &rewrite,
            issue,
            None,
            None,
            None,
            self.inner.config.plugins.tags,
        );
        Ok(output::ok(
            &CreateRedmineIssueOutput {
                success: true,
                issue: issue_out,
            },
            self.output_caps(),
        ))
    }

    /// `PUT /issues/{id}.json`, then a follow-up `GET`.
    #[tool(
        description = "Update fields on an existing issue, or add a note to its history. Use this when a field needs to change or a comment should be added; omit any parameter to leave that field unchanged. Write tool; blocked in read-only mode.",
        input_schema = crate::tools::schema::input::<UpdateRedmineIssueParams>(),
        output_schema = crate::tools::schema::output::<UpdateRedmineIssueOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn update_redmine_issue(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<UpdateRedmineIssueParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(notes) = &params.notes
            && notes.is_empty()
        {
            return Err(McpError::invalid_params(
                "notes must not be an empty string; omit the field to add no note",
                None,
            ));
        }
        let has_core_change = params.subject.is_some()
            || params.description.is_some()
            || params.tracker_id.is_some()
            || params.status_id.is_some()
            || params.priority_id.is_some()
            || params.assigned_to_id.is_some()
            || params.category_id.is_some()
            || params.fixed_version_id.is_some()
            || params.parent_issue_id.is_some()
            || params.start_date.is_some()
            || params.due_date.is_some()
            || params.done_ratio.is_some()
            || params.estimated_hours.is_some()
            || params.is_private.is_some()
            || params.notes.is_some()
            || params.tag_list.is_some()
            // uploads alone (no other field, no notes) is a legitimate
            // update, not a no-op.
            || params.uploads.as_ref().is_some_and(|u| !u.is_empty());
        let has_agile_change = params.story_points.is_some()
            || params.agile_sprint_id.is_some()
            || params.agile_position.is_some();
        if !has_core_change && !has_agile_change {
            return Err(McpError::invalid_params(
                "at least one field to change, or notes, must be given",
                None,
            ));
        }
        if has_agile_change && !self.inner.config.plugins.agile {
            return Ok(output::err(
                ErrorCode::Misconfigured,
                "story_points/agile_sprint_id/agile_position require the RedmineUP Agile plugin",
                Some("set REDMINE_AGILE_ENABLED=true, or omit these parameters"),
            ));
        }
        if params.tag_list.is_some() && !self.inner.config.plugins.tags {
            return Ok(output::err(
                ErrorCode::Misconfigured,
                "tag_list requires the AlphaNodes additional_tags plugin",
                Some("set REDMINE_TAGS_ENABLED=true, or omit tag_list"),
            ));
        }
        let tag_list = params.tag_list.map(validate_tag_list).transpose()?;

        let scoped = self.scoped(&ctx)?;
        let store = self.attachments();
        let (uploads, attachment_ids) = match resolve_and_mint_issue_uploads(
            &scoped,
            &self.inner.config.attachments.upload_file_roots,
            store.dir(),
            params.uploads,
        )
        .await
        {
            Ok(v) => v,
            Err(IssueUploadOutcome::Protocol(e)) => return Err(e),
            Err(IssueUploadOutcome::InBand(r)) => return Ok(r),
        };

        let patch = IssueUpdate {
            subject: params.subject,
            description: params.description,
            tracker_id: params.tracker_id,
            status_id: params.status_id,
            priority_id: params.priority_id,
            category_id: params.category_id,
            fixed_version_id: params.fixed_version_id,
            assigned_to_id: params.assigned_to_id.map(UserId),
            parent_issue_id: params.parent_issue_id.map(IssueId),
            start_date: params.start_date,
            due_date: params.due_date,
            done_ratio: params.done_ratio,
            estimated_hours: params.estimated_hours,
            is_private: params.is_private,
            notes: params.notes,
            private_notes: params.private_notes,
            uploads,
            tag_list,
        };

        // The core PUT and the agile PUT are separate requests to endpoints
        // with different validation: skip the core one when only agile
        // fields changed, rather than sending an empty patch.
        let mut issue = if has_core_change {
            match scoped.update_issue(IssueId(params.issue_id), &patch).await {
                Ok(issue) => issue,
                Err(e) => return Ok(to_tool_error(e)),
            }
        } else {
            match scoped.get_issue(IssueId(params.issue_id), &[]).await {
                Ok(issue) => issue,
                Err(e) => return Ok(to_tool_error(e)),
            }
        };
        if !attachment_ids.is_empty() {
            match fetch_attachments(&scoped, &attachment_ids).await {
                Ok(attachments) => issue.attachments = Some(attachments),
                Err(e) => return Ok(to_tool_error(e)),
            }
        }

        let mut agile_out: Option<AgileData> = None;
        if has_agile_change {
            let current = match scoped.get_agile_data(IssueId(params.issue_id)).await {
                Ok(row) => row,
                Err(e) => {
                    return Ok(agile_failure_result(e, has_core_change));
                }
            };
            let change = AgileChange {
                story_points: params.story_points,
                agile_sprint_id: params.agile_sprint_id,
                position: params.agile_position,
            };
            let merged = AgileDataAttributes::merge_over(current.as_ref(), &change);
            if let Err(e) = scoped
                .update_agile_data(IssueId(params.issue_id), &merged)
                .await
            {
                return Ok(agile_failure_result(e, has_core_change));
            }
            // Re-read so the response reflects Redmine's own ground truth,
            // not just the payload this call sent.
            agile_out = match scoped.get_agile_data(IssueId(params.issue_id)).await {
                Ok(row) => Some(row.unwrap_or_default()),
                Err(e) => {
                    return Ok(agile_failure_result(e, has_core_change));
                }
            };
        }

        let boundary = Boundary::new();
        let rewrite = self.content_url_rewrite();
        let issue_out = issue_detail_out(
            &boundary,
            &rewrite,
            issue,
            None,
            None,
            agile_out.as_ref(),
            self.inner.config.plugins.tags,
        );
        Ok(output::ok(
            &UpdateRedmineIssueOutput {
                success: true,
                issue: issue_out,
            },
            self.output_caps(),
        ))
    }

    /// Impact preview via `GET /issues/{id}.json?include=journals,attachments,relations`
    /// plus `GET /issues.json?parent_id={id}&status_id=*` and one
    /// `GET /time_entries.json?issue_id={id}&limit=1`, then (once confirmed)
    /// `DELETE /issues/{id}.json`.
    #[tool(
        description = "Delete a Redmine issue. Refuses by default and returns an impact preview (children/journals/attachments/relations/time-entry counts); pass confirm_delete=true to proceed, and confirm_delete_with_children=true too if it has subtasks. A refusal is a normal result, not an error. Use when the user explicitly asks to delete an issue. Write tool; blocked in read-only mode.",
        input_schema = crate::tools::schema::input::<DeleteRedmineIssueParams>(),
        output_schema = crate::tools::schema::output::<DeleteRedmineIssueOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn delete_redmine_issue(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<DeleteRedmineIssueParams>,
    ) -> Result<CallToolResult, McpError> {
        let issue_id = IssueId(params.issue_id);
        let scoped = self.scoped(&ctx)?;

        let issue = match scoped
            .get_issue(
                issue_id,
                &[
                    IssueInclude::Journals,
                    IssueInclude::Attachments,
                    IssueInclude::Relations,
                ],
            )
            .await
        {
            Ok(issue) => issue,
            Err(e) => return Ok(to_tool_error(e)),
        };
        let children_count = match scoped.list_subtasks(issue_id).await {
            Ok(children) => u64::try_from(children.len()).unwrap_or(u64::MAX),
            Err(e) => return Ok(to_tool_error(e)),
        };
        let time_entries_count = match scoped
            .list_time_entries_page(
                &TimeEntryQuery {
                    issue_id: Some(issue_id),
                    ..TimeEntryQuery::default()
                },
                1,
                0,
            )
            .await
        {
            Ok(page) => page.total_count,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let impact = DeleteImpactOut {
            children_count,
            journals_count: u64::try_from(issue.journals.map_or(0, |j| j.len()))
                .unwrap_or(u64::MAX),
            attachments_count: u64::try_from(issue.attachments.map_or(0, |a| a.len()))
                .unwrap_or(u64::MAX),
            relations_count: u64::try_from(issue.relations.map_or(0, |r| r.len()))
                .unwrap_or(u64::MAX),
            time_entries_count,
        };

        if !params.confirm_delete {
            return Ok(output::ok(
                &DeleteRedmineIssueOutput {
                    success: false,
                    error: Some(
                        "deletion requires confirm_delete=true; nothing has been deleted"
                            .to_string(),
                    ),
                    code: Some(ErrorCode::ConfirmationRequired),
                    hint: Some(
                        "review impact, then retry with confirm_delete=true (and confirm_delete_with_children=true if children_count > 0)"
                            .to_string(),
                    ),
                    impact: Some(impact),
                    deleted_issue_id: None,
                    cascade_deleted: None,
                },
                self.output_caps(),
            ));
        }
        if impact.children_count > 0 && !params.confirm_delete_with_children {
            return Ok(output::ok(
                &DeleteRedmineIssueOutput {
                    success: false,
                    error: Some(format!(
                        "issue has {} direct subtask(s); deleting it cascade-deletes them all, with no way to keep them",
                        impact.children_count
                    )),
                    code: Some(ErrorCode::ChildrenPresent),
                    hint: Some(
                        "retry with confirm_delete_with_children=true to proceed, or reassign/detach the subtasks first"
                            .to_string(),
                    ),
                    impact: Some(impact),
                    deleted_issue_id: None,
                    cascade_deleted: None,
                },
                self.output_caps(),
            ));
        }

        if let Err(e) = scoped.delete_issue(issue_id).await {
            return Ok(to_tool_error(e));
        }

        Ok(output::ok(
            &DeleteRedmineIssueOutput {
                success: true,
                error: None,
                code: None,
                hint: None,
                impact: None,
                deleted_issue_id: Some(params.issue_id),
                cascade_deleted: Some(impact.children_count),
            },
            self.output_caps(),
        ))
    }

    /// `GET /issues/{id}.json`, `POST /issues.json` for the root copy, then
    /// (bounded) `GET /issues.json?parent_id=...&status_id=*` +
    /// `POST /issues.json` per subtask, and optionally one
    /// `POST /issues/{source_id}/relations.json`.
    #[tool(
        description = "Copy an issue to a new one, optionally into another project, optionally recursively copying subtasks. Most fields are copied from the source unless overridden; status is never copied. Attachments are never copied. Bounded to 50 issues per call. Use this instead of create_redmine_issue when duplicating an existing issue. Write tool; blocked in read-only mode.",
        input_schema = crate::tools::schema::input::<CopyIssueParams>(),
        output_schema = crate::tools::schema::output::<CopyIssueOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn copy_issue(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CopyIssueParams>,
    ) -> Result<CallToolResult, McpError> {
        const COPY_MAX_TOTAL: usize = 50;
        const COPY_MAX_DEPTH: u32 = 5;

        let scoped = self.scoped(&ctx)?;
        let source = match scoped.get_issue(IssueId(params.issue_id), &[]).await {
            Ok(issue) => issue,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let target_project = match params.project_id {
            Some(project_ref) => resolve_project_ref(project_ref)?,
            None => ProjectIdent::Id(ProjectId(source.project.id)),
        };

        let root_create = IssueCreate {
            project_id: target_project.clone(),
            subject: params.subject.unwrap_or_else(|| source.subject.clone()),
            tracker_id: Some(params.tracker_id.unwrap_or(source.tracker.id)),
            status_id: None,
            priority_id: Some(params.priority_id.unwrap_or(source.priority.id)),
            category_id: params
                .category_id
                .or_else(|| source.category.as_ref().map(|c| c.id)),
            fixed_version_id: params
                .fixed_version_id
                .or_else(|| source.fixed_version.as_ref().map(|v| v.id)),
            description: params.description.or_else(|| source.description.clone()),
            assigned_to_id: params
                .assigned_to_id
                .map(UserId)
                .or_else(|| source.assigned_to.as_ref().map(|u| UserId(u.id))),
            parent_issue_id: None,
            start_date: source.start_date,
            due_date: source.due_date,
            done_ratio: source.done_ratio,
            estimated_hours: source.estimated_hours,
            is_private: source.is_private,
            uploads: Vec::new(),
            tag_list: None,
        };

        let created = match scoped.create_issue(&root_create).await {
            Ok(issue) => issue,
            Err(e) => return Ok(to_tool_error(e)),
        };

        if params.link_original {
            // Best-effort: the copy itself already succeeded, and a relation
            // failure (e.g. cross-project relations disabled) should not
            // make the tool report the whole copy as failed.
            let _ = scoped
                .create_relation(
                    IssueId(params.issue_id),
                    &IssueRelationCreate {
                        issue_to_id: IssueId(created.id),
                        relation_type: Some("copied_to".to_string()),
                        delay: None,
                    },
                )
                .await;
        }

        let mut subtasks_copied: u64 = 0;
        let mut subtasks_truncated = false;
        let mut total_created: usize = 1;

        if params.copy_subtasks {
            let mut queue: std::collections::VecDeque<(IssueId, IssueId, u32)> =
                std::collections::VecDeque::new();
            queue.push_back((IssueId(params.issue_id), IssueId(created.id), 0));

            'outer: while let Some((source_id, new_parent_id, depth)) = queue.pop_front() {
                if depth >= COPY_MAX_DEPTH {
                    subtasks_truncated = true;
                    continue;
                }
                let Ok(children) = scoped.list_subtasks(source_id).await else {
                    subtasks_truncated = true;
                    continue;
                };
                for child in children {
                    if total_created >= COPY_MAX_TOTAL {
                        subtasks_truncated = true;
                        break 'outer;
                    }
                    let child_create = IssueCreate {
                        project_id: target_project.clone(),
                        subject: child.subject.clone(),
                        tracker_id: Some(child.tracker.id),
                        status_id: None,
                        priority_id: Some(child.priority.id),
                        category_id: child.category.as_ref().map(|c| c.id),
                        fixed_version_id: child.fixed_version.as_ref().map(|v| v.id),
                        description: child.description.clone(),
                        assigned_to_id: child.assigned_to.as_ref().map(|u| UserId(u.id)),
                        parent_issue_id: Some(new_parent_id),
                        start_date: child.start_date,
                        due_date: child.due_date,
                        done_ratio: child.done_ratio,
                        estimated_hours: child.estimated_hours,
                        is_private: child.is_private,
                        uploads: Vec::new(),
                        tag_list: None,
                    };
                    match scoped.create_issue(&child_create).await {
                        Ok(new_child) => {
                            subtasks_copied = subtasks_copied.saturating_add(1);
                            total_created = total_created.saturating_add(1);
                            queue.push_back((
                                IssueId(child.id),
                                IssueId(new_child.id),
                                depth.saturating_add(1),
                            ));
                        }
                        Err(_) => {
                            subtasks_truncated = true;
                        }
                    }
                }
            }
        }

        let boundary = Boundary::new();
        let rewrite = self.content_url_rewrite();
        // `copy_issue` is create-like — see `create_redmine_issue`'s own note
        // on why it never touches the agile plugin.
        let issue_out = issue_detail_out(
            &boundary,
            &rewrite,
            created,
            None,
            None,
            None,
            self.inner.config.plugins.tags,
        );
        Ok(output::ok(
            &CopyIssueOutput {
                success: true,
                issue: issue_out,
                subtasks_copied,
                subtasks_truncated,
            },
            self.output_caps(),
        ))
    }

    /// `action="list"`: `GET /issues/{issue_id}/relations.json`.
    /// `action="create"`: `POST /issues/{issue_id}/relations.json`.
    /// `action="delete"`: `DELETE /relations/{relation_id}.json`.
    #[tool(
        description = "Manage relations between issues (relates, blocks, precedes, ...). Use this to list (issue_id, works read-only), create (issue_id+issue_to_id), or delete (relation_id) a relation. create/delete are blocked in read-only mode.",
        input_schema = crate::tools::schema::input::<ManageIssueRelationParams>(),
        output_schema = crate::tools::schema::output::<ManageIssueRelationOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn manage_issue_relation(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ManageIssueRelationParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(rt) = &params.relation_type
            && !RELATION_TYPES.contains(&rt.as_str())
        {
            return Err(McpError::invalid_params(
                format!(
                    "relation_type must be one of {}, got {rt:?}",
                    RELATION_TYPES.join(", ")
                ),
                None,
            ));
        }

        let scoped = self.scoped(&ctx)?;

        match params.action {
            ManageIssueRelationAction::List => {
                let issue_id = params.issue_id.ok_or_else(|| {
                    McpError::invalid_params("issue_id is required for action=\"list\"", None)
                })?;
                let relations = match scoped.list_relations(IssueId(issue_id)).await {
                    Ok(relations) => relations,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageIssueRelationOutput {
                        success: true,
                        relation: None,
                        relations: Some(relations.iter().map(relation_out).collect()),
                        deleted_relation_id: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageIssueRelationAction::Create => {
                if self.inner.config.read_only {
                    return Ok(output::err(
                        ErrorCode::ReadOnly,
                        "this server is running in read-only mode; manage_issue_relation(action=\"create\") is disabled",
                        Some(
                            "use action=\"list\" instead, or ask the operator to disable read-only mode",
                        ),
                    ));
                }
                let issue_id = params.issue_id.ok_or_else(|| {
                    McpError::invalid_params("issue_id is required for action=\"create\"", None)
                })?;
                let issue_to_id = params.issue_to_id.ok_or_else(|| {
                    McpError::invalid_params("issue_to_id is required for action=\"create\"", None)
                })?;
                let new = IssueRelationCreate {
                    issue_to_id: IssueId(issue_to_id),
                    relation_type: params.relation_type,
                    delay: params.delay,
                };
                let relation = match scoped.create_relation(IssueId(issue_id), &new).await {
                    Ok(relation) => relation,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageIssueRelationOutput {
                        success: true,
                        relation: Some(relation_out(&relation)),
                        relations: None,
                        deleted_relation_id: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageIssueRelationAction::Delete => {
                if self.inner.config.read_only {
                    return Ok(output::err(
                        ErrorCode::ReadOnly,
                        "this server is running in read-only mode; manage_issue_relation(action=\"delete\") is disabled",
                        Some(
                            "use action=\"list\" instead, or ask the operator to disable read-only mode",
                        ),
                    ));
                }
                let relation_id = params.relation_id.ok_or_else(|| {
                    McpError::invalid_params("relation_id is required for action=\"delete\"", None)
                })?;
                if let Err(e) = scoped.delete_relation(RelationId(relation_id)).await {
                    return Ok(to_tool_error(e));
                }
                Ok(output::ok(
                    &ManageIssueRelationOutput {
                        success: true,
                        relation: None,
                        relations: None,
                        deleted_relation_id: Some(relation_id),
                    },
                    self.output_caps(),
                ))
            }
        }
    }

    /// `action="add"`: `POST /issues/{issue_id}/watchers.json`.
    /// `action="remove"`: `DELETE /issues/{issue_id}/watchers/{user_id}.json`.
    #[tool(
        description = "Add or remove a watcher on an issue. Use this to subscribe/unsubscribe a user to an issue's notifications. Write tool; blocked in read-only mode.",
        input_schema = crate::tools::schema::input::<ManageIssueWatcherParams>(),
        output_schema = crate::tools::schema::output::<ManageIssueWatcherOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn manage_issue_watcher(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ManageIssueWatcherParams>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let issue_id = IssueId(params.issue_id);
        let user_id = UserId(params.user_id);

        let result = match params.action {
            ManageIssueWatcherAction::Add => scoped.add_watcher(issue_id, user_id).await,
            ManageIssueWatcherAction::Remove => scoped.remove_watcher(issue_id, user_id).await,
        };
        if let Err(e) = result {
            return Ok(to_tool_error(e));
        }

        Ok(output::ok(
            &ManageIssueWatcherOutput {
                success: true,
                issue_id: params.issue_id,
                user_id: params.user_id,
            },
            self.output_caps(),
        ))
    }

    /// `PUT /journals/{id}.json`.
    #[tool(
        description = "Edit an issue note's text and/or private flag. Use this to edit (journal_id+notes; empty string clears it) or set_private (journal_id+is_private) alone. journal_id comes from get_redmine_issue or get_private_notes. Write tool; blocked in read-only mode.",
        input_schema = crate::tools::schema::input::<ManageIssueNoteParams>(),
        output_schema = crate::tools::schema::output::<ManageIssueNoteOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn manage_issue_note(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ManageIssueNoteParams>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let journal_id = JournalId(params.journal_id);

        match params.action {
            ManageIssueNoteAction::Edit => {
                let notes = params.notes.ok_or_else(|| {
                    McpError::invalid_params("notes is required for action=\"edit\"", None)
                })?;
                let patch = JournalUpdate {
                    notes: Some(notes.clone()),
                    private_notes: params.private_notes,
                };
                if let Err(e) = scoped.update_journal(journal_id, &patch).await {
                    return Ok(to_tool_error(e));
                }
                Ok(output::ok(
                    &ManageIssueNoteOutput {
                        success: true,
                        journal_id: params.journal_id,
                        notes: Some(notes),
                        private_notes: params.private_notes,
                    },
                    self.output_caps(),
                ))
            }
            ManageIssueNoteAction::SetPrivate => {
                let is_private = params.is_private.ok_or_else(|| {
                    McpError::invalid_params(
                        "is_private is required for action=\"set_private\"",
                        None,
                    )
                })?;
                let patch = JournalUpdate {
                    notes: None,
                    private_notes: Some(is_private),
                };
                if let Err(e) = scoped.update_journal(journal_id, &patch).await {
                    return Ok(to_tool_error(e));
                }
                Ok(output::ok(
                    &ManageIssueNoteOutput {
                        success: true,
                        journal_id: params.journal_id,
                        notes: None,
                        private_notes: Some(is_private),
                    },
                    self.output_caps(),
                ))
            }
        }
    }

    /// `action="list"`: `GET /projects/{id}/issue_categories.json`.
    /// `action="create"`: `POST /projects/{id}/issue_categories.json`.
    /// `action="update"`: `PUT /issue_categories/{id}.json`.
    /// `action="delete"`: `DELETE /issue_categories/{id}.json`.
    #[tool(
        description = "Manage issue categories on a project. Use this to list (project_id, works read-only), create (project_id+name), update, or delete (category_id) categories; delete accepts reassign_to_id to move issues instead of leaving them uncategorised. create/update/delete are blocked in read-only mode.",
        input_schema = crate::tools::schema::input::<ManageIssueCategoryParams>(),
        output_schema = crate::tools::schema::output::<ManageIssueCategoryOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn manage_issue_category(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ManageIssueCategoryParams>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let boundary = Boundary::new();

        match params.action {
            ManageIssueCategoryAction::List => {
                let project_ref = params.project_id.ok_or_else(|| {
                    McpError::invalid_params("project_id is required for action=\"list\"", None)
                })?;
                let project_id = resolve_project_ref(project_ref)?;
                let categories = match scoped.list_issue_categories(&project_id).await {
                    Ok(categories) => categories,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageIssueCategoryOutput {
                        success: true,
                        category: None,
                        categories: Some(
                            categories
                                .iter()
                                .map(|c| issue_category_out(&boundary, c))
                                .collect(),
                        ),
                        deleted_category_id: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageIssueCategoryAction::Create => {
                if self.inner.config.read_only {
                    return Ok(output::err(
                        ErrorCode::ReadOnly,
                        "this server is running in read-only mode; manage_issue_category(action=\"create\") is disabled",
                        Some(
                            "use action=\"list\" instead, or ask the operator to disable read-only mode",
                        ),
                    ));
                }
                let project_ref = params.project_id.ok_or_else(|| {
                    McpError::invalid_params("project_id is required for action=\"create\"", None)
                })?;
                let project_id = resolve_project_ref(project_ref)?;
                let name = params
                    .name
                    .filter(|n| !n.trim().is_empty())
                    .ok_or_else(|| {
                        McpError::invalid_params(
                            "name is required (and must not be blank) for action=\"create\"",
                            None,
                        )
                    })?;
                let new = IssueCategoryCreate {
                    name,
                    assigned_to_id: params.assigned_to_id.map(UserId),
                };
                let category = match scoped.create_issue_category(&project_id, &new).await {
                    Ok(category) => category,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageIssueCategoryOutput {
                        success: true,
                        category: Some(issue_category_out(&boundary, &category)),
                        categories: None,
                        deleted_category_id: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageIssueCategoryAction::Update => {
                if self.inner.config.read_only {
                    return Ok(output::err(
                        ErrorCode::ReadOnly,
                        "this server is running in read-only mode; manage_issue_category(action=\"update\") is disabled",
                        Some(
                            "use action=\"list\" instead, or ask the operator to disable read-only mode",
                        ),
                    ));
                }
                let category_id = params.category_id.ok_or_else(|| {
                    McpError::invalid_params("category_id is required for action=\"update\"", None)
                })?;
                if let Some(name) = &params.name
                    && name.trim().is_empty()
                {
                    return Err(McpError::invalid_params(
                        "name must not be blank if given",
                        None,
                    ));
                }
                let patch = IssueCategoryUpdate {
                    name: params.name,
                    assigned_to_id: params.assigned_to_id.map(UserId),
                };
                let category = match scoped
                    .update_issue_category(IssueCategoryId(category_id), &patch)
                    .await
                {
                    Ok(category) => category,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageIssueCategoryOutput {
                        success: true,
                        category: Some(issue_category_out(&boundary, &category)),
                        categories: None,
                        deleted_category_id: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageIssueCategoryAction::Delete => {
                if self.inner.config.read_only {
                    return Ok(output::err(
                        ErrorCode::ReadOnly,
                        "this server is running in read-only mode; manage_issue_category(action=\"delete\") is disabled",
                        Some(
                            "use action=\"list\" instead, or ask the operator to disable read-only mode",
                        ),
                    ));
                }
                let category_id = params.category_id.ok_or_else(|| {
                    McpError::invalid_params("category_id is required for action=\"delete\"", None)
                })?;
                if let Err(e) = scoped
                    .delete_issue_category(
                        IssueCategoryId(category_id),
                        params.reassign_to_id.map(IssueCategoryId),
                    )
                    .await
                {
                    return Ok(to_tool_error(e));
                }
                Ok(output::ok(
                    &ManageIssueCategoryOutput {
                        success: true,
                        category: None,
                        categories: None,
                        deleted_category_id: Some(category_id),
                    },
                    self.output_caps(),
                ))
            }
        }
    }
}

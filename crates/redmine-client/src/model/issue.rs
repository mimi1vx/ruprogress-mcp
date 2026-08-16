//! `GET/POST/PUT /issues`.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::attachment::Attachment;
use super::custom_field::CustomFieldWrite;
use super::journal::Journal;
use super::plugins::tags::IssueTag;
use super::relation::IssueRelation;
use super::upload::UploadRef;
use super::{
    Collection, CustomField, IdName, IdOnly, permissive_datetime, permissive_datetime_opt,
};
use crate::ids::{IssueId, ProjectIdent, UserId};

/// A Redmine issue.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    /// The issue id.
    pub id: u64,
    /// The project this issue belongs to.
    pub project: IdName,
    /// The tracker (Bug, Feature, ...).
    pub tracker: IdName,
    /// The current status.
    pub status: IdName,
    /// The priority.
    pub priority: IdName,
    /// Who created the issue.
    pub author: IdName,
    /// Who the issue is assigned to, if anyone.
    #[serde(default)]
    pub assigned_to: Option<IdName>,
    /// The parent issue, if this is a sub-issue. Redmine sends only the id
    /// here (`{"id": N}`, no `name`), unlike every other association on an
    /// issue — see [`IdOnly`].
    #[serde(default)]
    pub parent: Option<IdOnly>,
    /// The issue category, if one is set.
    #[serde(default)]
    pub category: Option<IdName>,
    /// The target version (roadmap milestone), if one is set.
    #[serde(default)]
    pub fixed_version: Option<IdName>,
    /// The issue subject line.
    pub subject: String,
    /// The issue description.
    #[serde(default)]
    pub description: Option<String>,
    /// Percent done, 0-100.
    #[serde(default)]
    pub done_ratio: Option<u8>,
    /// Whether the issue is private.
    #[serde(default)]
    pub is_private: Option<bool>,
    /// Estimated hours.
    #[serde(default)]
    pub estimated_hours: Option<f64>,
    /// Hours logged against this issue. Redmine omits this field entirely
    /// (not merely `null`) when the credential lacks the `view_time_entries`
    /// permission — `None` here is therefore ambiguous between "zero hours
    /// logged" and "not visible to this credential", matching Redmine's own
    /// ambiguity rather than inventing a distinction Redmine itself doesn't
    /// make.
    #[serde(default)]
    pub spent_hours: Option<f64>,
    /// Planned start date.
    #[serde(default)]
    pub start_date: Option<NaiveDate>,
    /// Planned due date.
    #[serde(default)]
    pub due_date: Option<NaiveDate>,
    /// When the issue was created.
    #[serde(deserialize_with = "permissive_datetime")]
    pub created_on: DateTime<Utc>,
    /// When the issue was last updated.
    #[serde(deserialize_with = "permissive_datetime")]
    pub updated_on: DateTime<Utc>,
    /// When the issue was closed, if it is closed.
    #[serde(default, deserialize_with = "permissive_datetime_opt")]
    pub closed_on: Option<DateTime<Utc>>,
    /// Custom field values attached to this issue.
    #[serde(default)]
    pub custom_fields: Option<Vec<CustomField>>,
    /// Journal entries (notes and field-change history). `None` when
    /// `include=journals` was not requested; `Some(vec![])` when requested
    /// and the issue has none. Same convention as `Project.trackers`.
    #[serde(default)]
    pub journals: Option<Vec<Journal>>,
    /// File attachments. `None` = not requested, `Some(vec![])` = none.
    #[serde(default)]
    pub attachments: Option<Vec<Attachment>>,
    /// Relations to other issues. `None` = not requested, `Some(vec![])` =
    /// none.
    #[serde(default)]
    pub relations: Option<Vec<IssueRelation>>,
    /// Users watching this issue. `None` = not requested, `Some(vec![])` =
    /// none. Redmine additionally hides this array entirely (rather than
    /// sending an empty one) when the credential lacks
    /// `view_issue_watchers` — indistinguishable here from "not requested".
    #[serde(default)]
    pub watchers: Option<Vec<IdName>>,
    /// Direct sub-issues, recursively nested one level deep. `None` = not
    /// requested, `Some(vec![])` = none (a leaf issue).
    #[serde(default)]
    pub children: Option<Vec<IssueChild>>,
    /// `AlphaNodes` `additional_tags` plugin tags, when the plugin injects
    /// the key. `None` is ambiguous: it means either the plugin is not
    /// installed/enabled, or the caller lacks `view_issue_tags` — Redmine
    /// itself makes no distinction, so this client doesn't invent one.
    #[serde(default)]
    pub tags: Option<Vec<IssueTag>>,
}

/// One level of `Issue.children`. Redmine's own `render_api_issue_children`
/// nests arbitrarily deep; this client stops at two levels total (this type
/// plus [`IssueChildLeaf`]). A grandchild beyond that is simply absent from the JSON, not truncated with
/// a signal: a caller who needs the full tree uses `list_subtasks`
/// recursively, one level at a time.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct IssueChild {
    /// The child issue's id.
    pub id: u64,
    /// The child's tracker, if visible.
    #[serde(default)]
    pub tracker: Option<IdName>,
    /// The child's subject.
    #[serde(default)]
    pub subject: String,
    /// The child's own children (grandchildren of the original issue).
    #[serde(default)]
    pub children: Option<Vec<IssueChildLeaf>>,
}

/// The deepest level of nesting under `Issue.children` this client models
/// (see [`IssueChild`]). Carries no further `children`
/// field at all, by design.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct IssueChildLeaf {
    /// The grandchild issue's id.
    pub id: u64,
    /// The grandchild's tracker, if visible.
    #[serde(default)]
    pub tracker: Option<IdName>,
    /// The grandchild's subject.
    #[serde(default)]
    pub subject: String,
}

/// Payload for `POST /issues.json`. Separate from [`Issue`] because
/// `Issue` is `#[non_exhaustive]` and its field set genuinely differs
/// (`project_id` vs `project: IdName`, etc).
#[derive(Debug, Clone, Serialize)]
pub struct IssueCreate {
    /// The project to create the issue in.
    pub project_id: ProjectIdent,
    /// The issue subject.
    pub subject: String,
    /// The tracker id, if not the project's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracker_id: Option<u64>,
    /// The status id, if not the tracker's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_id: Option<u64>,
    /// The priority id, if not the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority_id: Option<u64>,
    /// The category id, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_id: Option<u64>,
    /// The target version (roadmap milestone) id, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed_version_id: Option<u64>,
    /// The description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Who to assign the issue to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_to_id: Option<UserId>,
    /// The parent issue id, to create a sub-issue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_issue_id: Option<IssueId>,
    /// Planned start date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_date: Option<NaiveDate>,
    /// Planned due date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<NaiveDate>,
    /// Percent done, 0-100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_ratio: Option<u8>,
    /// Estimated hours.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_hours: Option<f64>,
    /// Whether the issue is private.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_private: Option<bool>,
    /// Files to attach as part of this same request, via upload tokens
    /// already obtained from `POST /uploads.json`. Empty by
    /// default, in which case the key is omitted entirely.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uploads: Vec<UploadRef>,
    /// The issue's initial tags (`AlphaNodes` `additional_tags` plugin).
    /// There is no "existing set" on create, so this is simply the tags to
    /// start with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_list: Option<Vec<String>>,
    /// Custom field values to set on creation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_fields: Option<Vec<CustomFieldWrite>>,
}

impl IssueCreate {
    /// The two fields Redmine always requires; everything else defaults to
    /// "use the project/tracker default" via `None`.
    #[must_use]
    pub fn new(project_id: ProjectIdent, subject: impl Into<String>) -> Self {
        Self {
            project_id,
            subject: subject.into(),
            tracker_id: None,
            status_id: None,
            priority_id: None,
            category_id: None,
            fixed_version_id: None,
            description: None,
            assigned_to_id: None,
            parent_issue_id: None,
            start_date: None,
            due_date: None,
            done_ratio: None,
            estimated_hours: None,
            is_private: None,
            uploads: Vec::new(),
            tag_list: None,
            custom_fields: None,
        }
    }
}

/// Payload for `PUT /issues/{id}.json`. All fields optional: only those set
/// are changed. `notes` adds a journal entry without changing any field.
/// There is no supported way to *clear* `assigned_to_id`/`category_id`/
/// `fixed_version_id`/`parent_issue_id` back to unset through this type —
/// Redmine accepts an empty string for that over the wire, but this client
/// only ever sends a present value or omits the field entirely.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IssueUpdate {
    /// New subject, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// New description, if changing it. An empty string clears it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// New tracker id, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracker_id: Option<u64>,
    /// New status id, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_id: Option<u64>,
    /// New priority id, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority_id: Option<u64>,
    /// New category id, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_id: Option<u64>,
    /// New target version id, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed_version_id: Option<u64>,
    /// New assignee, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_to_id: Option<UserId>,
    /// New parent issue, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_issue_id: Option<IssueId>,
    /// New planned start date, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_date: Option<NaiveDate>,
    /// New planned due date, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<NaiveDate>,
    /// New done ratio, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_ratio: Option<u8>,
    /// New estimated hours, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_hours: Option<f64>,
    /// New privacy flag, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_private: Option<bool>,
    /// A journal note to add, independent of any field change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Whether the note added via `notes` (if any) is private. Ignored by
    /// Redmine when `notes` is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_notes: Option<bool>,
    /// Files to attach as part of this same request, via upload tokens
    /// already obtained from `POST /uploads.json`. Empty by
    /// default, in which case the key is omitted entirely.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uploads: Vec<UploadRef>,
    /// New tags, replacing the whole set (`AlphaNodes` `additional_tags`
    /// plugin). `Some(vec![])` clears every tag; `None` leaves the set
    /// unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_list: Option<Vec<String>>,
    /// Custom field values to set, if changing any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_fields: Option<Vec<CustomFieldWrite>>,
}

/// `include=` values accepted by the issue endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueInclude {
    /// Sub-issues.
    Children,
    /// File attachments.
    Attachments,
    /// Related issues.
    Relations,
    /// Associated changesets.
    Changesets,
    /// Journal entries (notes and field-change history).
    Journals,
    /// Watchers.
    Watchers,
    /// Statuses this issue could transition to for the current user.
    AllowedStatuses,
}

impl IssueInclude {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Children => "children",
            Self::Attachments => "attachments",
            Self::Relations => "relations",
            Self::Changesets => "changesets",
            Self::Journals => "journals",
            Self::Watchers => "watchers",
            Self::AllowedStatuses => "allowed_statuses",
        }
    }
}

/// `status_id=` filter values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFilter {
    /// Only open issues (Redmine's default).
    Open,
    /// Only closed issues.
    Closed,
    /// All issues regardless of status.
    All,
    /// A specific status id.
    Id(u64),
}

impl StatusFilter {
    fn as_query_value(self) -> String {
        match self {
            Self::Open => "open".to_string(),
            Self::Closed => "closed".to_string(),
            Self::All => "*".to_string(),
            Self::Id(id) => id.to_string(),
        }
    }
}

/// `assigned_to_id=` filter values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserFilter {
    /// The requesting user.
    Me,
    /// A specific user id.
    Id(UserId),
}

impl UserFilter {
    pub(crate) fn as_query_value(self) -> String {
        match self {
            Self::Me => "me".to_string(),
            Self::Id(id) => id.0.to_string(),
        }
    }
}

/// Filter and sort parameters for `GET /issues.json`.
#[derive(Debug, Default, Clone)]
pub struct IssueQuery {
    /// Restrict to one project.
    pub project: Option<ProjectIdent>,
    /// Status filter.
    pub status: Option<StatusFilter>,
    /// Assignee filter.
    pub assigned_to: Option<UserFilter>,
    /// Redmine operator syntax, e.g. `">=2026-01-01"`.
    pub updated_on: Option<String>,
    /// Sort column(s), Redmine's own syntax (e.g. `"updated_on:desc"`).
    pub sort: Option<String>,
    /// Anything the typed fields above don't cover: `cf_12`, `subproject_id`, ...
    pub extra: BTreeMap<String, String>,
}

impl IssueQuery {
    /// Convert to the query-parameter map sent on the wire.
    #[must_use]
    pub fn to_query(&self) -> crate::client::Query {
        let mut q = crate::client::Query::default();
        if let Some(project) = &self.project {
            q.insert("project_id", project.to_string());
        }
        if let Some(status) = self.status {
            q.insert("status_id", status.as_query_value());
        }
        if let Some(assigned_to) = self.assigned_to {
            q.insert("assigned_to_id", assigned_to.as_query_value());
        }
        if let Some(updated_on) = &self.updated_on {
            q.insert("updated_on", updated_on.clone());
        }
        if let Some(sort) = &self.sort {
            q.insert("sort", sort.clone());
        }
        for (k, v) in &self.extra {
            q.insert(k.clone(), v.clone());
        }
        q
    }
}

/// Build the `include=a,b,c` query value for a slice of includes.
pub(crate) fn includes_to_query_value(includes: &[IssueInclude]) -> Option<String> {
    if includes.is_empty() {
        return None;
    }
    Some(
        includes
            .iter()
            .map(|i| i.as_str())
            .collect::<Vec<_>>()
            .join(","),
    )
}

#[derive(Debug, Deserialize)]
pub(crate) struct IssueEnvelope {
    pub issue: Issue,
}

#[derive(Debug, Serialize)]
pub(crate) struct IssueCreateEnvelope<'a> {
    pub issue: &'a IssueCreate,
}

#[derive(Debug, Serialize)]
pub(crate) struct IssueUpdateEnvelope<'a> {
    pub issue: &'a IssueUpdate,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IssuesEnvelope {
    issues: Vec<Issue>,
    total_count: u64,
    offset: u64,
    limit: u32,
}

impl Collection for IssuesEnvelope {
    type Item = Issue;

    fn total_count(&self) -> u64 {
        self.total_count
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn limit(&self) -> u32 {
        self.limit
    }

    fn into_items(self) -> Vec<Issue> {
        self.issues
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const FIXTURE_6_1: &str = include_str!("../../tests/fixtures/issue_6_1.json");
    const FIXTURE_7_0: &str = include_str!("../../tests/fixtures/issue_7_0.json");

    #[test]
    fn round_trips_against_6_1_fixture() {
        let env: IssueEnvelope =
            serde_json::from_str(FIXTURE_6_1).expect("6.1 fixture should parse");
        assert_eq!(env.issue.subject, "Example issue");
    }

    #[test]
    fn round_trips_against_7_0_fixture() {
        let env: IssueEnvelope =
            serde_json::from_str(FIXTURE_7_0).expect("7.0 fixture should parse");
        assert_eq!(env.issue.subject, "Example issue");
    }

    #[test]
    fn unknown_top_level_field_does_not_fail() {
        let json = r#"{"issue": {
            "id": 1, "project": {"id":1,"name":"P"}, "tracker": {"id":1,"name":"Bug"},
            "status": {"id":1,"name":"New"}, "priority": {"id":1,"name":"Normal"},
            "author": {"id":1,"name":"A"}, "subject": "s",
            "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z",
            "a_field_from_a_future_redmine_version": 42
        }}"#;
        let env: IssueEnvelope =
            serde_json::from_str(json).expect("unknown field must not fail parsing");
        assert_eq!(env.issue.id, 1);
    }

    #[test]
    fn issue_create_serializes_only_set_fields() {
        let create = IssueCreate::new(
            ProjectIdent::Identifier("demo".parse().unwrap()),
            "New issue",
        );
        let value = serde_json::to_value(IssueCreateEnvelope { issue: &create }).unwrap();
        let obj = value
            .get("issue")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert_eq!(obj["subject"], "New issue");
        assert!(!obj.contains_key("tracker_id"));
        assert!(!obj.contains_key("uploads"));
    }

    #[test]
    fn issue_create_serializes_uploads_when_set() {
        let mut create = IssueCreate::new(
            ProjectIdent::Identifier("demo".parse().unwrap()),
            "New issue",
        );
        create.uploads = vec![UploadRef {
            token: "42.abcdef0123456789".to_string(),
            description: Some("a report".to_string()),
        }];
        let value = serde_json::to_value(IssueCreateEnvelope { issue: &create }).unwrap();
        let uploads = value
            .get("issue")
            .and_then(|issue| issue.get("uploads"))
            .unwrap();
        assert_eq!(
            *uploads,
            serde_json::json!([{"token": "42.abcdef0123456789", "description": "a report"}])
        );
    }

    #[test]
    fn issue_update_omits_uploads_when_empty() {
        let patch = IssueUpdate::default();
        let value = serde_json::to_value(IssueUpdateEnvelope { issue: &patch }).unwrap();
        let obj = value
            .get("issue")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert!(!obj.contains_key("uploads"));
    }

    #[test]
    fn issue_create_omits_tag_list_when_none() {
        let create = IssueCreate::new(
            ProjectIdent::Identifier("demo".parse().unwrap()),
            "New issue",
        );
        let value = serde_json::to_value(IssueCreateEnvelope { issue: &create }).unwrap();
        let obj = value
            .get("issue")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert!(!obj.contains_key("tag_list"));
    }

    #[test]
    fn issue_update_empty_tag_list_serializes_as_an_empty_array_not_an_omitted_key() {
        let patch = IssueUpdate {
            tag_list: Some(vec![]),
            ..Default::default()
        };
        let value = serde_json::to_value(IssueUpdateEnvelope { issue: &patch }).unwrap();
        let tag_list = value
            .get("issue")
            .and_then(|issue| issue.get("tag_list"))
            .unwrap();
        assert_eq!(*tag_list, serde_json::json!([]));
    }

    #[test]
    fn issue_update_tag_list_with_entries_serializes_the_full_replacement_set() {
        let patch = IssueUpdate {
            tag_list: Some(vec!["a".to_string(), "b".to_string()]),
            ..Default::default()
        };
        let value = serde_json::to_value(IssueUpdateEnvelope { issue: &patch }).unwrap();
        let tag_list = value
            .get("issue")
            .and_then(|issue| issue.get("tag_list"))
            .unwrap();
        assert_eq!(*tag_list, serde_json::json!(["a", "b"]));
    }

    #[test]
    fn issue_parses_tags_with_and_without_an_id() {
        let env: IssueEnvelope =
            serde_json::from_str(include_str!("../../tests/fixtures/issue_with_tags.json"))
                .expect("fixture should parse");
        let tags = env.issue.tags.expect("tags key should be present");
        assert_eq!(tags.len(), 2);
        let with_id = tags.first().expect("first tag");
        assert_eq!(with_id.id, Some(3));
        assert_eq!(with_id.name, "urgent");
        let without_id = tags.get(1).expect("second tag");
        assert_eq!(without_id.id, None);
        assert_eq!(without_id.name, "needs-review");
    }

    #[test]
    fn issue_without_a_tags_key_has_no_tags() {
        let env: IssueEnvelope =
            serde_json::from_str(FIXTURE_6_1).expect("6.1 fixture should parse");
        assert!(env.issue.tags.is_none());
    }

    #[test]
    fn parent_deserializes_from_an_id_only_object_with_no_name() {
        // `issues/show.api.rsb`: `api.parent(:id => @issue.parent_id)` sends
        // no `name` — deserializing this into an `IdName` (which requires
        // one) would be a decode error.
        let json = r#"{"issue": {
            "id": 2, "project": {"id":1,"name":"P"}, "tracker": {"id":1,"name":"Bug"},
            "status": {"id":1,"name":"New"}, "priority": {"id":1,"name":"Normal"},
            "author": {"id":1,"name":"A"}, "subject": "sub-issue",
            "parent": {"id": 100},
            "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
        }}"#;
        let env: IssueEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(env.issue.parent.expect("parent").id, 100);
    }

    #[test]
    fn every_include_gated_field_defaults_to_none_when_absent() {
        let env: IssueEnvelope =
            serde_json::from_str(FIXTURE_6_1).expect("6.1 fixture should parse");
        let issue = env.issue;
        assert!(issue.category.is_none());
        assert!(issue.fixed_version.is_none());
        assert!(issue.spent_hours.is_none());
        assert!(issue.journals.is_none());
        assert!(issue.attachments.is_none());
        assert!(issue.relations.is_none());
        assert!(issue.watchers.is_none());
        assert!(issue.children.is_none());
    }

    #[test]
    fn parses_every_include_gated_field_and_two_levels_of_children() {
        let json = r#"{"issue": {
            "id": 1, "project": {"id":1,"name":"P"}, "tracker": {"id":1,"name":"Bug"},
            "status": {"id":1,"name":"New"}, "priority": {"id":1,"name":"Normal"},
            "author": {"id":1,"name":"A"}, "subject": "s",
            "category": {"id": 9, "name": "Backend"},
            "fixed_version": {"id": 6, "name": "v2.0"},
            "spent_hours": 3.5,
            "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z",
            "journals": [{"id": 1, "notes": "hi", "created_on": "2026-01-01T00:00:00Z"}],
            "attachments": [{"id": 1, "filename": "a.png", "filesize": 10,
                "content_url": "https://x/a.png", "created_on": "2026-01-01T00:00:00Z"}],
            "relations": [{"id": 1, "issue_id": 1, "issue_to_id": 2, "relation_type": "relates"}],
            "watchers": [{"id": 5, "name": "Alice"}],
            "children": [{"id": 2, "subject": "child", "tracker": {"id": 1, "name": "Bug"},
                "children": [{"id": 3, "subject": "grandchild"}]}]
        }}"#;
        let env: IssueEnvelope = serde_json::from_str(json).expect("should parse");
        let issue = env.issue;
        assert_eq!(issue.category.unwrap().name, "Backend");
        assert_eq!(issue.fixed_version.unwrap().name, "v2.0");
        assert_eq!(issue.spent_hours, Some(3.5));
        assert_eq!(issue.journals.unwrap().len(), 1);
        assert_eq!(issue.attachments.unwrap().len(), 1);
        assert_eq!(issue.relations.unwrap().len(), 1);
        assert_eq!(issue.watchers.unwrap().len(), 1);
        let children = issue.children.unwrap();
        assert_eq!(children.len(), 1);
        let child = children.first().expect("one child");
        assert_eq!(child.id, 2);
        let grandchildren = child.children.as_ref().unwrap();
        assert_eq!(grandchildren.len(), 1);
        assert_eq!(grandchildren.first().expect("one grandchild").id, 3);
    }
}

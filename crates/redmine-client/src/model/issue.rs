//! `GET/POST/PUT /issues`.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::{Collection, CustomField, IdName, permissive_datetime, permissive_datetime_opt};
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
    /// The parent issue, if this is a sub-issue.
    #[serde(default)]
    pub parent: Option<IdName>,
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
            description: None,
            assigned_to_id: None,
            parent_issue_id: None,
            start_date: None,
            due_date: None,
        }
    }
}

/// Payload for `PUT /issues/{id}.json`. All fields optional: only those set
/// are changed. `notes` adds a journal entry without changing any field.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IssueUpdate {
    /// New subject, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// New status id, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_id: Option<u64>,
    /// New assignee, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_to_id: Option<UserId>,
    /// New done ratio, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_ratio: Option<u8>,
    /// A journal note to add, independent of any field change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
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
    fn as_query_value(self) -> String {
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
    }
}

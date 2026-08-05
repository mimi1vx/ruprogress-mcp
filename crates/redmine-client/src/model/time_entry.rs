//! `GET/POST /time_entries`.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::{Collection, IdName, permissive_datetime};
use crate::ids::{IssueId, ProjectId, UserId};

/// A bare `{"id": N}` reference, as Redmine sends for `time_entry.issue`
/// (unlike most other embedded references, it carries no `name`).
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct IssueRef {
    /// The referenced issue's id.
    pub id: u64,
}

/// A Redmine time entry.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct TimeEntry {
    /// The time entry id.
    pub id: u64,
    /// The project this time was logged against.
    pub project: IdName,
    /// The issue this time was logged against, if any.
    #[serde(default)]
    pub issue: Option<IssueRef>,
    /// Who logged the time.
    pub user: IdName,
    /// The time-tracking activity (Development, QA, ...).
    pub activity: IdName,
    /// Hours logged.
    pub hours: f64,
    /// Free-text comment.
    #[serde(default)]
    pub comments: Option<String>,
    /// The date the time was spent on.
    pub spent_on: NaiveDate,
    /// When this entry was created.
    #[serde(deserialize_with = "permissive_datetime")]
    pub created_on: DateTime<Utc>,
    /// When this entry was last updated.
    #[serde(deserialize_with = "permissive_datetime")]
    pub updated_on: DateTime<Utc>,
}

/// Payload for `POST /time_entries.json`. Exactly one of `issue_id` /
/// `project_id` must be set — enforced by Redmine, not by this type.
#[derive(Debug, Clone, Serialize)]
pub struct TimeEntryCreate {
    /// Log against this issue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<IssueId>,
    /// Log against this project directly (no issue).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    /// The date the time was spent on; Redmine defaults to today if omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spent_on: Option<NaiveDate>,
    /// Hours logged.
    pub hours: f64,
    /// The activity id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_id: Option<u64>,
    /// Free-text comment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments: Option<String>,
}

impl TimeEntryCreate {
    /// Log `hours` against `issue_id`. Use the struct literal directly for
    /// project-only entries.
    #[must_use]
    pub fn for_issue(issue_id: IssueId, hours: f64) -> Self {
        Self {
            issue_id: Some(issue_id),
            project_id: None,
            spent_on: None,
            hours,
            activity_id: None,
            comments: None,
        }
    }
}

/// Filter parameters for `GET /time_entries.json`.
#[derive(Debug, Default, Clone)]
pub struct TimeEntryQuery {
    /// Restrict to one project.
    pub project_id: Option<ProjectId>,
    /// Restrict to one issue.
    pub issue_id: Option<IssueId>,
    /// Restrict to one user.
    pub user_id: Option<UserId>,
    /// Redmine operator syntax, e.g. `"><2026-01-01|2026-01-31"`.
    pub spent_on: Option<String>,
    /// Anything not covered by a typed field.
    pub extra: BTreeMap<String, String>,
}

impl TimeEntryQuery {
    /// Convert to the query-parameter map sent on the wire.
    #[must_use]
    pub fn to_query(&self) -> crate::client::Query {
        let mut q = crate::client::Query::default();
        if let Some(project_id) = self.project_id {
            q.insert("project_id", project_id.to_string());
        }
        if let Some(issue_id) = self.issue_id {
            q.insert("issue_id", issue_id.to_string());
        }
        if let Some(user_id) = self.user_id {
            q.insert("user_id", user_id.to_string());
        }
        if let Some(spent_on) = &self.spent_on {
            q.insert("spent_on", spent_on.clone());
        }
        for (k, v) in &self.extra {
            q.insert(k.clone(), v.clone());
        }
        q
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct TimeEntryEnvelope {
    pub time_entry: TimeEntry,
}

#[derive(Debug, Serialize)]
pub(crate) struct TimeEntryCreateEnvelope<'a> {
    pub time_entry: &'a TimeEntryCreate,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TimeEntriesEnvelope {
    time_entries: Vec<TimeEntry>,
    total_count: u64,
    offset: u64,
    limit: u32,
}

impl Collection for TimeEntriesEnvelope {
    type Item = TimeEntry;

    fn total_count(&self) -> u64 {
        self.total_count
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn limit(&self) -> u32 {
        self.limit
    }

    fn into_items(self) -> Vec<TimeEntry> {
        self.time_entries
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const FIXTURE_6_1: &str = include_str!("../../tests/fixtures/time_entry_6_1.json");
    const FIXTURE_7_0: &str = include_str!("../../tests/fixtures/time_entry_7_0.json");

    #[test]
    fn round_trips_against_6_1_fixture() {
        let env: TimeEntryEnvelope =
            serde_json::from_str(FIXTURE_6_1).expect("6.1 fixture should parse");
        assert!((env.time_entry.hours - 2.5).abs() < f64::EPSILON);
    }

    #[test]
    fn round_trips_against_7_0_fixture() {
        let env: TimeEntryEnvelope =
            serde_json::from_str(FIXTURE_7_0).expect("7.0 fixture should parse");
        assert!((env.time_entry.hours - 2.5).abs() < f64::EPSILON);
    }
}

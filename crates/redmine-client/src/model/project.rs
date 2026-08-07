//! `GET/POST/PUT /projects`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{Collection, CustomField, IdName, permissive_datetime};

/// A Redmine project.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    /// The project id.
    pub id: u64,
    /// The project's display name.
    pub name: String,
    /// The project's slug identifier (used in URLs).
    pub identifier: String,
    /// The project description.
    #[serde(default)]
    pub description: Option<String>,
    /// Redmine's numeric status (1 = active, 5 = closed, 9 = archived).
    #[serde(default)]
    pub status: Option<u8>,
    /// Whether the project is publicly visible.
    #[serde(default)]
    pub is_public: Option<bool>,
    /// The parent project, if this is a sub-project.
    #[serde(default)]
    pub parent: Option<IdName>,
    /// When the project was created.
    #[serde(deserialize_with = "permissive_datetime")]
    pub created_on: DateTime<Utc>,
    /// When the project was last updated.
    #[serde(deserialize_with = "permissive_datetime")]
    pub updated_on: DateTime<Utc>,
    /// Custom field values attached to this project.
    #[serde(default)]
    pub custom_fields: Option<Vec<CustomField>>,
    /// Trackers enabled for this project. `None` means trackers were not
    /// requested (no `include=trackers` on the request that produced this
    /// value) — **not** that none are enabled; `Some(vec![])` means none
    /// are enabled.
    #[serde(default)]
    pub trackers: Option<Vec<IdName>>,
    /// Modules enabled for this project. Same `None` = not requested,
    /// `Some(vec![])` = none enabled convention as `trackers` above —
    /// populated only when `include=enabled_modules` was requested.
    #[serde(default)]
    pub enabled_modules: Option<Vec<IdName>>,
}

/// `include=` values accepted by the project endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectInclude {
    /// Trackers enabled for this project.
    Trackers,
    /// Issue categories.
    IssueCategories,
    /// Enabled modules.
    EnabledModules,
    /// Time-tracking activities.
    TimeEntryActivities,
}

impl ProjectInclude {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Trackers => "trackers",
            Self::IssueCategories => "issue_categories",
            Self::EnabledModules => "enabled_modules",
            Self::TimeEntryActivities => "time_entry_activities",
        }
    }
}

pub(crate) fn includes_to_query_value(includes: &[ProjectInclude]) -> Option<String> {
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

/// Filter parameters for `GET /projects.json`.
#[derive(Debug, Default, Clone)]
pub struct ProjectQuery {
    /// Anything not covered by a typed field, e.g. `cf_3`.
    pub extra: BTreeMap<String, String>,
}

impl ProjectQuery {
    /// Convert to the query-parameter map sent on the wire.
    #[must_use]
    pub fn to_query(&self) -> crate::client::Query {
        let mut q = crate::client::Query::default();
        for (k, v) in &self.extra {
            q.insert(k.clone(), v.clone());
        }
        q
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProjectEnvelope {
    pub project: Project,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProjectsEnvelope {
    projects: Vec<Project>,
    total_count: u64,
    offset: u64,
    limit: u32,
}

impl Collection for ProjectsEnvelope {
    type Item = Project;

    fn total_count(&self) -> u64 {
        self.total_count
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn limit(&self) -> u32 {
        self.limit
    }

    fn into_items(self) -> Vec<Project> {
        self.projects
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const FIXTURE_6_1: &str = include_str!("../../tests/fixtures/project_6_1.json");
    const FIXTURE_7_0: &str = include_str!("../../tests/fixtures/project_7_0.json");

    #[test]
    fn round_trips_against_6_1_fixture() {
        let env: ProjectEnvelope =
            serde_json::from_str(FIXTURE_6_1).expect("6.1 fixture should parse");
        assert_eq!(env.project.identifier, "example-project");
    }

    #[test]
    fn round_trips_against_7_0_fixture() {
        let env: ProjectEnvelope =
            serde_json::from_str(FIXTURE_7_0).expect("7.0 fixture should parse");
        assert_eq!(env.project.identifier, "example-project");
    }

    const FIXTURE_WITH_TRACKERS_7_0: &str =
        include_str!("../../tests/fixtures/project_with_trackers_7_0.json");

    #[test]
    fn trackers_is_none_when_not_requested() {
        let env: ProjectEnvelope =
            serde_json::from_str(FIXTURE_7_0).expect("7.0 fixture should parse");
        assert!(env.project.trackers.is_none());
    }

    #[test]
    fn trackers_is_populated_when_include_trackers_was_requested() {
        let env: ProjectEnvelope = serde_json::from_str(FIXTURE_WITH_TRACKERS_7_0)
            .expect("project_with_trackers fixture should parse");
        let trackers = env.project.trackers.expect("trackers should be Some");
        assert_eq!(trackers.len(), 2);
        assert_eq!(trackers.first().unwrap().name, "Bug");
    }

    #[test]
    fn unknown_top_level_field_does_not_fail() {
        let json = r#"{"project": {
            "id": 1, "name": "P", "identifier": "p",
            "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z",
            "a_field_from_a_future_redmine_version": 42
        }}"#;
        let env: ProjectEnvelope =
            serde_json::from_str(json).expect("unknown field must not fail parsing");
        assert_eq!(env.project.id, 1);
    }
}

//! `GET /projects/{id}/versions` (roadmap targets).

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;

use super::{IdName, permissive_datetime};

/// A Redmine version (roadmap target).
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct Version {
    /// The version id.
    pub id: u64,
    /// The owning project.
    pub project: IdName,
    /// The version name.
    pub name: String,
    /// The version description.
    #[serde(default)]
    pub description: Option<String>,
    /// `"open"`, `"locked"`, or `"closed"`.
    pub status: String,
    /// The target due date.
    #[serde(default)]
    pub due_date: Option<NaiveDate>,
    /// Cross-project sharing mode, if any.
    #[serde(default)]
    pub sharing: Option<String>,
    /// When the version was created.
    #[serde(deserialize_with = "permissive_datetime")]
    pub created_on: DateTime<Utc>,
    /// When the version was last updated.
    #[serde(deserialize_with = "permissive_datetime")]
    pub updated_on: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[allow(
    dead_code,
    reason = "model exists for round-trip tests; no API method uses it yet"
)]
pub(crate) struct VersionEnvelope {
    pub version: Version,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // Inline fixture (not a captured-instance file): Version has no
    // dedicated API method yet, so this exercises the
    // model shape only. See tests/fixtures/README.md for the fixture policy
    // that applies to models with real API methods.
    const JSON: &str = r#"{"version": {
        "id": 1, "project": {"id": 1, "name": "Example"}, "name": "1.0",
        "status": "open", "due_date": "2026-12-31",
        "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z"
    }}"#;

    #[test]
    fn round_trips() {
        let env: VersionEnvelope = serde_json::from_str(JSON).expect("should parse");
        assert_eq!(env.version.name, "1.0");
    }

    #[test]
    fn unknown_field_does_not_fail() {
        let json = r#"{"version": {
            "id": 1, "project": {"id": 1, "name": "Example"}, "name": "1.0", "status": "open",
            "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-01T00:00:00Z",
            "wiki_page_title": "future field"
        }}"#;
        let env: VersionEnvelope = serde_json::from_str(json).expect("unknown field must not fail");
        assert_eq!(env.version.id, 1);
    }
}

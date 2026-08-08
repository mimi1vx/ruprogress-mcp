//! `GET/POST /projects/{id}/versions`, `GET/PUT/DELETE /versions/{id}`
//! (roadmap targets).

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

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
pub(crate) struct VersionEnvelope {
    pub version: Version,
}

/// `GET /projects/{id}/versions.json`'s response shape: `{"versions": [...],
/// "total_count": N}`. `total_count` is deliberately **not** a field here —
/// the endpoint has no `limit`/`offset` support at all (verified against
/// `VersionsController#index`'s `format.api` branch, which always returns
/// `@project.shared_versions.to_a` in full) and always equals the returned
/// array's length, so there is nothing to reconcile it against. This is
/// neither a [`super::Collection`] (no `offset`/`limit`) nor a
/// [`super::BareCollection`] (does carry `total_count`). Models are never
/// `deny_unknown_fields`, so the extra field is silently ignored rather than
/// needing to be named here.
#[derive(Debug, Deserialize)]
pub(crate) struct VersionsEnvelope {
    pub versions: Vec<Version>,
}

/// `status` values accepted by `POST`/`PUT` and `list_redmine_versions`'s
/// client-side filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VersionStatus {
    /// The default on create.
    Open,
    /// No new issues may target this version.
    Locked,
    /// The version is complete.
    Closed,
}

/// `sharing` values accepted by `POST`/`PUT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SharingMode {
    /// Not shared outside this project (the default on create).
    None,
    /// Shared with sub-projects.
    Descendants,
    /// Shared within the project hierarchy.
    Hierarchy,
    /// Shared with the whole project tree.
    Tree,
    /// Shared instance-wide.
    System,
}

/// The `version` hash sent to `POST /projects/{id}/versions.json` and
/// `PUT /versions/{id}.json` — the same shape for both (Redmine's PUT
/// accepts every field `POST` does).
#[derive(Debug, Clone, Default, Serialize)]
pub struct VersionWrite {
    /// The version name. Required by Redmine on create; omit on update to
    /// leave unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The version description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `open`/`locked`/`closed`. Defaults to `open` on create if omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<VersionStatus>,
    /// Target due date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<NaiveDate>,
    /// Cross-project sharing mode. Defaults to `none` on create if omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sharing: Option<SharingMode>,
    /// Associated wiki page title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wiki_page_title: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct VersionWriteEnvelope<'a> {
    pub version: &'a VersionWrite,
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

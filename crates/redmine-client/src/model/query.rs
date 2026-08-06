//! `GET /queries.json` — Redmine's saved (custom) issue queries.
//!
//! Not to be confused with this crate's own filter/sort builders (e.g.
//! [`crate::model::issue::IssueQuery`]); this is the *Redmine resource*
//! representing a query a user saved in the web UI.

use serde::Deserialize;

/// A saved Redmine query.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct SavedQuery {
    /// The saved query's id.
    pub id: u64,
    /// The saved query's display name.
    pub name: String,
    /// Whether other users can see this query.
    #[serde(default)]
    pub is_public: Option<bool>,
    /// The project this query is scoped to, if any (`None` = global).
    #[serde(default)]
    pub project_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[allow(
    dead_code,
    reason = "model exists for round-trip tests; no API method uses it yet"
)]
pub(crate) struct SavedQueriesEnvelope {
    pub queries: Vec<SavedQuery>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // Inline fixture: see tests/fixtures/README.md for the policy that
    // applies to models with a real API method.
    const JSON: &str = r#"{"queries": [
        {"id": 1, "name": "My open issues", "is_public": false, "project_id": 1}
    ]}"#;

    #[test]
    fn round_trips() {
        let env: SavedQueriesEnvelope = serde_json::from_str(JSON).expect("should parse");
        assert_eq!(env.queries.first().unwrap().name, "My open issues");
    }
}

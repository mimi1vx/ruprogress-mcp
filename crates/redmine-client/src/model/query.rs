//! `GET /queries.json` — Redmine's saved (custom) issue queries.
//!
//! Not to be confused with this crate's own filter/sort builders (e.g.
//! [`crate::model::issue::IssueQuery`]); this is the *Redmine resource*
//! representing a query a user saved in the web UI.

use serde::Deserialize;

use super::Collection;

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
pub(crate) struct SavedQueriesEnvelope {
    queries: Vec<SavedQuery>,
    total_count: u64,
    offset: u64,
    limit: u32,
}

impl Collection for SavedQueriesEnvelope {
    type Item = SavedQuery;

    fn total_count(&self) -> u64 {
        self.total_count
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn limit(&self) -> u32 {
        self.limit
    }

    fn into_items(self) -> Vec<SavedQuery> {
        self.queries
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const FIXTURE_6_1: &str = include_str!("../../tests/fixtures/saved_queries_6_1.json");
    const FIXTURE_7_0: &str = include_str!("../../tests/fixtures/saved_queries_7_0.json");

    #[test]
    fn round_trips_against_6_1_fixture() {
        let env: SavedQueriesEnvelope =
            serde_json::from_str(FIXTURE_6_1).expect("6.1 fixture should parse");
        assert_eq!(env.queries.first().unwrap().name, "My open issues");
        assert_eq!(env.total_count, 1);
    }

    #[test]
    fn round_trips_against_7_0_fixture() {
        let env: SavedQueriesEnvelope =
            serde_json::from_str(FIXTURE_7_0).expect("7.0 fixture should parse");
        assert_eq!(env.queries.first().unwrap().name, "My open issues");
    }
}

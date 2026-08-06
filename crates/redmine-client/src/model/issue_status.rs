//! `GET /issue_statuses.json` — no pagination envelope.

use serde::Deserialize;

use super::BareCollection;

/// A Redmine issue status.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct IssueStatus {
    /// The status id.
    pub id: u64,
    /// The status's display name.
    pub name: String,
    /// Whether an issue in this status counts as closed. `Option` because a
    /// defensive parse, even though every supported version emits it.
    #[serde(default)]
    pub is_closed: Option<bool>,
    /// Whether this is the default status for a new issue. `Option` because
    /// Redmine dropped this field from this endpoint in some versions.
    #[serde(default)]
    pub is_default: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IssueStatusesEnvelope {
    issue_statuses: Vec<IssueStatus>,
}

impl BareCollection for IssueStatusesEnvelope {
    type Item = IssueStatus;

    fn into_items(self) -> Vec<IssueStatus> {
        self.issue_statuses
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const FIXTURE_6_1: &str = include_str!("../../tests/fixtures/issue_status_6_1.json");
    const FIXTURE_7_0: &str = include_str!("../../tests/fixtures/issue_status_7_0.json");

    #[test]
    fn round_trips_against_6_1_fixture_with_is_default() {
        let env: IssueStatusesEnvelope =
            serde_json::from_str(FIXTURE_6_1).expect("6.1 fixture should parse");
        let first = env.issue_statuses.first().unwrap();
        assert_eq!(first.name, "New");
        assert_eq!(first.is_default, Some(true));
    }

    #[test]
    fn round_trips_against_7_0_fixture_without_is_default() {
        let env: IssueStatusesEnvelope =
            serde_json::from_str(FIXTURE_7_0).expect("7.0 fixture should parse");
        let first = env.issue_statuses.first().unwrap();
        assert_eq!(first.is_closed, Some(false));
        assert_eq!(first.is_default, None);
    }
}

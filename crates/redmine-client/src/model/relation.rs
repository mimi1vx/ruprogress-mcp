//! Issue relations, embedded in an [`crate::model::issue::Issue`] via
//! `include=relations`. `manage_issue_relation` (4b-write) will add the
//! standalone `POST`/`DELETE /relations.json` methods; only the read shape
//! is needed here.

use serde::Deserialize;

/// A relation between two issues (`relates`, `blocks`, `precedes`, ...).
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct IssueRelation {
    /// The relation id.
    pub id: u64,
    /// The source issue id. Redmine's wire field is `issue_id`, populated
    /// from the relation's `issue_from_id` — do not confuse with
    /// `issue_to_id`.
    pub issue_id: u64,
    /// The target issue id.
    pub issue_to_id: u64,
    /// `relates`, `duplicates`, `duplicated`, `blocks`, `blocked`,
    /// `precedes`, `follows`, `copied_to`, or `copied_from`.
    pub relation_type: String,
    /// Delay in days. Only meaningful for `precedes`/`follows`.
    #[serde(default)]
    pub delay: Option<i64>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let json = r#"{
            "id": 5, "issue_id": 123, "issue_to_id": 456,
            "relation_type": "blocks", "delay": null
        }"#;
        let relation: IssueRelation = serde_json::from_str(json).expect("should parse");
        assert_eq!(relation.issue_id, 123);
        assert_eq!(relation.issue_to_id, 456);
        assert_eq!(relation.relation_type, "blocks");
        assert_eq!(relation.delay, None);
    }

    #[test]
    fn unknown_field_does_not_fail_parsing() {
        let json = r#"{
            "id": 5, "issue_id": 1, "issue_to_id": 2,
            "relation_type": "relates", "future_field": true
        }"#;
        let relation: IssueRelation = serde_json::from_str(json).expect("should parse");
        assert_eq!(relation.id, 5);
    }
}

//! Issue relations: the read shape embedded in an
//! [`crate::model::issue::Issue`] via `include=relations` or returned by
//! `GET /issues/{id}/relations.json`, plus the `POST`/`DELETE` payloads
//! `manage_issue_relation` uses.

use serde::{Deserialize, Serialize};

use super::BareCollection;
use crate::ids::IssueId;

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

/// Payload for `POST /issues/{issue_id}/relations.json`. `relation_type`
/// defaults to `"relates"` on Redmine's side when omitted; `delay` is
/// silently discarded by Redmine for every `relation_type` other than
/// `precedes` (`IssueRelation#handle_issue_order`), not rejected.
#[derive(Debug, Clone, Serialize)]
pub struct IssueRelationCreate {
    /// The target issue id.
    pub issue_to_id: IssueId,
    /// One of `relates`, `duplicates`, `duplicated`, `blocks`, `blocked`,
    /// `precedes`, `follows`, `copied_to`, `copied_from`. Defaults to
    /// `relates` when omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation_type: Option<String>,
    /// Delay in days. Only meaningful for `precedes`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct IssueRelationCreateEnvelope<'a> {
    pub relation: &'a IssueRelationCreate,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IssueRelationEnvelope {
    pub relation: IssueRelation,
}

/// `GET /issues/{issue_id}/relations.json` carries **no** pagination
/// envelope at all — not even `total_count` (verified against
/// `issue_relations/index.api.rsb`, which has no `api_meta` call).
#[derive(Debug, Deserialize)]
pub(crate) struct IssueRelationsEnvelope {
    relations: Vec<IssueRelation>,
}

impl BareCollection for IssueRelationsEnvelope {
    type Item = IssueRelation;

    fn into_items(self) -> Vec<IssueRelation> {
        self.relations
    }
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

    #[test]
    fn relations_envelope_has_no_pagination_fields() {
        let json = r#"{"relations": [
            {"id": 1, "issue_id": 9, "issue_to_id": 7, "relation_type": "relates", "delay": null}
        ]}"#;
        let env: IssueRelationsEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(env.into_items().len(), 1);
    }

    #[test]
    fn create_omits_unset_relation_type_and_delay() {
        let create = IssueRelationCreate {
            issue_to_id: IssueId(7),
            relation_type: None,
            delay: None,
        };
        let value =
            serde_json::to_value(IssueRelationCreateEnvelope { relation: &create }).unwrap();
        let obj = value
            .get("relation")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert_eq!(obj["issue_to_id"], 7);
        assert!(!obj.contains_key("relation_type"));
        assert!(!obj.contains_key("delay"));
    }
}

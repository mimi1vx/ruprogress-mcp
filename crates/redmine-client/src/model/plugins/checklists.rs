//! `RedmineUP` Checklists Pro: `GET/POST /issues/{id}/checklists.json`,
//! `PUT /checklists/{id}.json`.
//!
//! Synthetic models derived from the reference implementation's handling of
//! this plugin, not a live capture — Checklists Pro is commercial. See
//! `tests/fixtures/README.md`'s plugin fixtures section.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use crate::model::{BareCollection, permissive_datetime_opt};

/// One checklist item (a checkable line, or a section header when
/// `is_section` is `true`). Every field but `id` is `#[serde(default)]`:
/// the reference implementation has observed plugin versions that omit
/// any of them.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct ChecklistItem {
    /// The checklist item id.
    #[serde(default)]
    pub id: u64,
    /// The item's text, or the section header's title.
    #[serde(default)]
    pub subject: String,
    /// Whether the item is checked. Meaningless for a section header (the
    /// plugin ignores it there rather than rejecting it).
    #[serde(default)]
    pub is_done: Option<bool>,
    /// `true` for a section header rather than a checkable item.
    #[serde(default)]
    pub is_section: Option<bool>,
    /// 1-based position within the issue's checklist.
    #[serde(default)]
    pub position: Option<u32>,
    /// When the item was created. Spelled `_at`, not `_on`, on the wire —
    /// unlike every other timestamp this client models — because it is the
    /// plugin's own spelling; the tool layer renames it to `created_on` in
    /// its output for consistency with the rest of the server.
    #[serde(default, deserialize_with = "permissive_datetime_opt")]
    pub created_at: Option<DateTime<Utc>>,
    /// When the item was last updated. See [`Self::created_at`] on the `_at`
    /// spelling.
    #[serde(default, deserialize_with = "permissive_datetime_opt")]
    pub updated_at: Option<DateTime<Utc>>,
}

/// Payload for `POST /issues/{id}/checklists.json`.
#[derive(Debug, Clone, Serialize)]
pub struct ChecklistItemCreate {
    /// The item's text, or the section header's title. Must not be blank.
    pub subject: String,
    /// `true` to create a section header rather than a checkable item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_section: Option<bool>,
    /// Initial checked state. Sent as given even when `is_section` is
    /// `true` — the plugin, not this client, decides to ignore it there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_done: Option<bool>,
    /// 1-based position. Omitted to append at the end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
}

/// Payload for `PUT /checklists/{id}.json`. Every field optional: only
/// those set are changed.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ChecklistItemUpdate {
    /// New text, if changing it. Must not be blank if given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// New checked state, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_done: Option<bool>,
    /// New 1-based position, if changing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChecklistItemCreateEnvelope<'a> {
    pub checklist: &'a ChecklistItemCreate,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChecklistItemUpdateEnvelope<'a> {
    pub checklist: &'a ChecklistItemUpdate,
}

/// `GET /issues/{id}/checklists.json` responds with either
/// `{"checklists": [...]}` or a bare `[...]` array — both shapes observed
/// by the reference implementation across plugin versions. A manual
/// `Deserialize` that peeks at the JSON node kind, same approach as
/// [`crate::model::custom_field::CustomFieldValue`]: `#[serde(untagged)]`
/// is banned in this crate (see that type's module doc) because its error
/// messages name no variant. A shape that is neither is a decode error
/// naming the endpoint, not a silent empty result.
#[derive(Debug)]
pub(crate) struct ChecklistItemsEnvelope(Vec<ChecklistItem>);

impl<'de> Deserialize<'de> for ChecklistItemsEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let items = match value {
            serde_json::Value::Array(_) => {
                serde_json::from_value(value).map_err(serde::de::Error::custom)?
            }
            serde_json::Value::Object(ref map) if map.contains_key("checklists") => {
                #[derive(Deserialize)]
                struct Envelope {
                    checklists: Vec<ChecklistItem>,
                }
                let env: Envelope =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                env.checklists
            }
            other => {
                return Err(serde::de::Error::custom(format!(
                    "GET .../checklists.json: expected a checklist item array or a \
                     {{\"checklists\": [...]}} envelope, got {other}"
                )));
            }
        };
        Ok(Self(items))
    }
}

impl BareCollection for ChecklistItemsEnvelope {
    type Item = ChecklistItem;

    fn into_items(self) -> Vec<ChecklistItem> {
        self.0
    }
}

/// `POST /issues/{id}/checklists.json` responds with either
/// `{"checklist": {"id": N}}` or `{"id": N}`; the reference implementation
/// notes the plugin does not reliably carry an id in the response body at
/// all, so neither shape being present is `None`, not a decode error.
#[derive(Debug)]
pub(crate) struct ChecklistItemCreated(pub(crate) Option<u64>);

impl<'de> Deserialize<'de> for ChecklistItemCreated {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let id = value
            .get("checklist")
            .and_then(|c| c.get("id"))
            .or_else(|| value.get("id"))
            .and_then(serde_json::Value::as_u64);
        Ok(Self(id))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    #[test]
    fn envelope_shape_parses() {
        let json = r#"{"checklists": [{"id": 1, "subject": "Write tests"}]}"#;
        let env: ChecklistItemsEnvelope = serde_json::from_str(json).expect("should parse");
        let items = env.into_items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].subject, "Write tests");
    }

    #[test]
    fn bare_array_shape_parses() {
        let json = r#"[{"id": 1, "subject": "Write tests"}]"#;
        let env: ChecklistItemsEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(env.into_items().len(), 1);
    }

    #[test]
    fn a_third_shape_is_a_decode_error_not_a_silent_empty_result() {
        let json = r#"{"unexpected": true}"#;
        let err = serde_json::from_str::<ChecklistItemsEnvelope>(json).unwrap_err();
        assert!(err.to_string().contains("checklists.json"));
    }

    #[test]
    fn item_missing_every_optional_field_still_parses() {
        let json = r#"{"id": 5}"#;
        let item: ChecklistItem = serde_json::from_str(json).expect("should parse");
        assert_eq!(item.id, 5);
        assert_eq!(item.subject, "");
        assert_eq!(item.is_done, None);
        assert_eq!(item.is_section, None);
        assert_eq!(item.position, None);
        assert_eq!(item.created_at, None);
        assert_eq!(item.updated_at, None);
    }

    #[test]
    fn created_response_reads_the_nested_checklist_shape() {
        let json = r#"{"checklist": {"id": 9}}"#;
        let created: ChecklistItemCreated = serde_json::from_str(json).expect("should parse");
        assert_eq!(created.0, Some(9));
    }

    #[test]
    fn created_response_reads_the_flat_shape() {
        let json = r#"{"id": 9}"#;
        let created: ChecklistItemCreated = serde_json::from_str(json).expect("should parse");
        assert_eq!(created.0, Some(9));
    }

    #[test]
    fn created_response_with_no_id_anywhere_is_none_not_an_error() {
        let json = r"{}";
        let created: ChecklistItemCreated = serde_json::from_str(json).expect("should parse");
        assert_eq!(created.0, None);
    }

    #[test]
    fn create_serializes_only_set_fields() {
        let create = ChecklistItemCreate {
            subject: "Write tests".to_string(),
            is_section: None,
            is_done: None,
            position: None,
        };
        let value =
            serde_json::to_value(ChecklistItemCreateEnvelope { checklist: &create }).unwrap();
        let obj = value
            .get("checklist")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert_eq!(obj["subject"], "Write tests");
        assert!(!obj.contains_key("is_section"));
        assert!(!obj.contains_key("is_done"));
        assert!(!obj.contains_key("position"));
    }

    #[test]
    fn update_serializes_only_set_fields() {
        let patch = ChecklistItemUpdate {
            subject: None,
            is_done: Some(true),
            position: None,
        };
        let value =
            serde_json::to_value(ChecklistItemUpdateEnvelope { checklist: &patch }).unwrap();
        let obj = value
            .get("checklist")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert_eq!(obj["is_done"], true);
        assert!(!obj.contains_key("subject"));
        assert!(!obj.contains_key("position"));
    }
}

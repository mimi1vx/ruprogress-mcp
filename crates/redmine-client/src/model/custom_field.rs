//! Custom field values.
//!
//! A custom field's `value` is a JSON string when the field is
//! single-valued and a JSON array of strings when `multiple = true`. This is
//! exactly the kind of shape `#[serde(untagged)]` handles badly (useless
//! error messages, silent wrong-arm selection on the least-specified part of
//! the Redmine API) — so this has a manual `Deserialize`. The write
//! direction (`impl Serialize`) has no such ambiguity to worry about: each
//! variant maps to exactly one JSON shape, so it derives no such caveat.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{BareCollection, IdName};

/// The value of a single Redmine custom field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomFieldValue {
    /// A single-valued field. `None` for an unset field (Redmine sends
    /// `null` or an empty string depending on field type).
    Single(Option<String>),
    /// A `multiple = true` field's values.
    Multiple(Vec<String>),
}

impl Serialize for CustomFieldValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Single(Some(s)) => serializer.serialize_str(s),
            Self::Single(None) => serializer.serialize_none(),
            Self::Multiple(values) => values.serialize(serializer),
        }
    }
}

/// One entry of a write-side `custom_fields` array:
/// `{"id": N, "value": ...}`. Shared by every tool that writes custom
/// fields — products today, issues from `7f` onward.
#[derive(Debug, Clone, Serialize)]
pub struct CustomFieldWrite {
    /// The custom field's id. Values are accepted by id only in this
    /// crate; resolving a field by name is a tool-layer concern (it needs a
    /// definitions lookup this crate has no opinion on).
    pub id: u64,
    /// The value to set.
    pub value: CustomFieldValue,
}

impl<'de> Deserialize<'de> for CustomFieldValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::Null => Ok(Self::Single(None)),
            serde_json::Value::String(s) => Ok(Self::Single(Some(s))),
            serde_json::Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        serde_json::Value::String(s) => out.push(s),
                        serde_json::Value::Null => {}
                        other => {
                            return Err(serde::de::Error::custom(format!(
                                "expected a string in a multi-value custom field, got {other}"
                            )));
                        }
                    }
                }
                Ok(Self::Multiple(out))
            }
            other => Err(serde::de::Error::custom(format!(
                "expected a custom field value (string, array of strings, or null), got {other}"
            ))),
        }
    }
}

/// A custom field as it appears attached to another resource (issue,
/// project, user, ...).
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct CustomField {
    /// The custom field's id.
    pub id: u64,
    /// The custom field's display name.
    pub name: String,
    /// The value, if Redmine included one.
    #[serde(default)]
    pub value: Option<CustomFieldValue>,
}

/// A single allowed value for a `field_format: "list"` custom field, as
/// Redmine's `GET /custom_fields.json` sends it (`{"value": ..., "label":
/// ...}`). `CustomFieldDefinition::possible_values` collapses these to their
/// `value`s.
#[derive(Debug, Clone, Deserialize)]
struct PossibleValue {
    value: String,
}

/// A custom field *definition*, as returned by the global, admin-only
/// `GET /custom_fields.json` — distinct from [`CustomField`], which is the
/// value attached to one resource. Redmine's `is_required` here is the
/// definition's own flag; workflow rules and per-tracker settings can still
/// make a field effectively required without it being reflected here (see
/// the reference tool-reference.md caveat on
/// `list_project_issue_custom_fields`).
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct CustomFieldDefinition {
    /// The custom field's id.
    pub id: u64,
    /// The custom field's display name.
    pub name: String,
    /// `"string"`, `"list"`, `"date"`, ...
    pub field_format: String,
    /// The definition's own required flag (see the struct-level caveat).
    #[serde(default)]
    pub is_required: Option<bool>,
    /// Whether multiple values may be selected.
    #[serde(default)]
    pub multiple: Option<bool>,
    /// The default value, if any.
    #[serde(default)]
    pub default_value: Option<String>,
    /// The allowed values, for `field_format: "list"` fields.
    #[serde(default, deserialize_with = "deserialize_possible_values")]
    pub possible_values: Option<Vec<String>>,
    /// `"issue"`, `"project"`, `"time_entry"`, `"user"`, `"version"`,
    /// `"group"`, ... `None` for older Redmine versions that omit it.
    #[serde(default)]
    pub customized_type: Option<String>,
    /// Whether this field applies to every project rather than a specific
    /// list.
    #[serde(default)]
    pub is_for_all: Option<bool>,
    /// The projects this field is scoped to, when `is_for_all` is `false`.
    /// Only present for `customized_type == "issue"`.
    #[serde(default)]
    pub projects: Option<Vec<IdName>>,
    /// The trackers this field applies to. Only present for
    /// `customized_type == "issue"`.
    #[serde(default)]
    pub trackers: Option<Vec<IdName>>,
}

fn deserialize_possible_values<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<Vec<PossibleValue>>::deserialize(deserializer)?;
    Ok(raw.map(|values| values.into_iter().map(|v| v.value).collect()))
}

/// `GET /custom_fields.json` carries no pagination envelope at all
/// (confirmed against `custom_fields/index.api.rsb`, which has no
/// `api_meta` call) — it is also admin-only on Redmine's side.
#[derive(Debug, Deserialize)]
pub(crate) struct CustomFieldDefinitionsEnvelope {
    custom_fields: Vec<CustomFieldDefinition>,
}

impl BareCollection for CustomFieldDefinitionsEnvelope {
    type Item = CustomFieldDefinition;

    fn into_items(self) -> Vec<CustomFieldDefinition> {
        self.custom_fields
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn parse(json: &str) -> CustomFieldValue {
        serde_json::from_str(json).expect("should deserialize")
    }

    #[test]
    fn single_string_value() {
        assert_eq!(
            parse(r#""blue""#),
            CustomFieldValue::Single(Some("blue".to_string()))
        );
    }

    #[test]
    fn null_value() {
        assert_eq!(parse("null"), CustomFieldValue::Single(None));
    }

    #[test]
    fn empty_string_value() {
        assert_eq!(
            parse(r#""""#),
            CustomFieldValue::Single(Some(String::new()))
        );
    }

    #[test]
    fn multiple_values() {
        assert_eq!(
            parse(r#"["a","b","c"]"#),
            CustomFieldValue::Multiple(vec!["a".to_string(), "b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn empty_array_is_multiple_with_no_values() {
        assert_eq!(parse("[]"), CustomFieldValue::Multiple(vec![]));
    }

    #[test]
    fn custom_field_round_trips_with_and_without_value() {
        let with_value: CustomField =
            serde_json::from_str(r#"{"id":1,"name":"Colour","value":"blue"}"#).unwrap();
        assert_eq!(
            with_value.value,
            Some(CustomFieldValue::Single(Some("blue".to_string())))
        );

        let without_value: CustomField =
            serde_json::from_str(r#"{"id":2,"name":"Empty"}"#).unwrap();
        assert_eq!(without_value.value, None);
    }

    #[test]
    fn custom_field_definitions_envelope_round_trips_and_flattens_possible_values() {
        let json = r#"{"custom_fields": [{
            "id": 6, "name": "Size", "field_format": "list", "is_required": false,
            "multiple": false, "default_value": "M",
            "possible_values": [{"value": "S", "label": "S"}, {"value": "M", "label": "M"}],
            "customized_type": "issue", "is_for_all": true,
            "trackers": [{"id": 5, "name": "Bug"}]
        }]}"#;
        let env: CustomFieldDefinitionsEnvelope = serde_json::from_str(json).expect("should parse");
        let field = env.custom_fields.first().unwrap();
        assert_eq!(
            field.possible_values,
            Some(vec!["S".to_string(), "M".to_string()])
        );
        assert_eq!(field.customized_type.as_deref(), Some("issue"));
    }

    #[test]
    fn custom_field_value_serializes_each_variant_to_its_wire_shape() {
        assert_eq!(
            serde_json::to_value(CustomFieldValue::Single(Some("blue".to_string()))).unwrap(),
            serde_json::json!("blue")
        );
        assert_eq!(
            serde_json::to_value(CustomFieldValue::Single(None)).unwrap(),
            serde_json::Value::Null
        );
        assert_eq!(
            serde_json::to_value(CustomFieldValue::Multiple(vec![
                "a".to_string(),
                "b".to_string()
            ]))
            .unwrap(),
            serde_json::json!(["a", "b"])
        );
    }

    #[test]
    fn custom_field_write_serializes_id_and_value() {
        let write = CustomFieldWrite {
            id: 7,
            value: CustomFieldValue::Single(Some("blue".to_string())),
        };
        let value = serde_json::to_value(write).unwrap();
        assert_eq!(value, serde_json::json!({"id": 7, "value": "blue"}));
    }

    #[test]
    fn custom_field_definition_without_possible_values_parses() {
        let json = r#"{"custom_fields": [{
            "id": 1, "name": "Notes", "field_format": "text"
        }]}"#;
        let env: CustomFieldDefinitionsEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(env.custom_fields.first().unwrap().possible_values, None);
    }
}

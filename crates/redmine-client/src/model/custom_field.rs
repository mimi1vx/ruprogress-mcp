//! Custom field values.
//!
//! A custom field's `value` is a JSON string when the field is
//! single-valued and a JSON array of strings when `multiple = true`. This is
//! exactly the kind of shape `#[serde(untagged)]` handles badly (useless
//! error messages, silent wrong-arm selection on the least-specified part of
//! the Redmine API) — so this has a manual `Deserialize`.

use serde::{Deserialize, Deserializer};

/// The value of a single Redmine custom field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomFieldValue {
    /// A single-valued field. `None` for an unset field (Redmine sends
    /// `null` or an empty string depending on field type).
    Single(Option<String>),
    /// A `multiple = true` field's values.
    Multiple(Vec<String>),
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
}

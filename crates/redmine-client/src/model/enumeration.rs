//! `GET /enumerations/*` (issue priorities, time-entry activities,
//! document categories — all share this shape).

use serde::Deserialize;

/// A single Redmine enumeration value.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct Enumeration {
    /// The enumeration value's id.
    pub id: u64,
    /// The display name.
    pub name: String,
    /// Whether this is the default value for its enumeration.
    #[serde(default)]
    pub is_default: Option<bool>,
    /// Whether this value is currently active (selectable).
    #[serde(default)]
    pub active: Option<bool>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // Inline fixture: see tests/fixtures/README.md for the policy that
    // applies to models with a real API method in phase 1's surface.
    const JSON: &str = r#"{"id": 1, "name": "Normal", "is_default": true, "active": true}"#;

    #[test]
    fn round_trips() {
        let value: Enumeration = serde_json::from_str(JSON).expect("should parse");
        assert_eq!(value.name, "Normal");
        assert_eq!(value.is_default, Some(true));
    }
}

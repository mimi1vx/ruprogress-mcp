//! `GET /enumerations/*` (issue priorities, time-entry activities,
//! document categories — all share this shape).

use serde::Deserialize;

use super::BareCollection;

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

/// `GET /enumerations/issue_priorities.json` — no pagination envelope.
#[derive(Debug, Deserialize)]
pub(crate) struct IssuePrioritiesEnvelope {
    issue_priorities: Vec<Enumeration>,
}

impl BareCollection for IssuePrioritiesEnvelope {
    type Item = Enumeration;

    fn into_items(self) -> Vec<Enumeration> {
        self.issue_priorities
    }
}

/// `GET /enumerations/time_entry_activities.json` — no pagination envelope.
#[derive(Debug, Deserialize)]
pub(crate) struct TimeEntryActivitiesEnvelope {
    time_entry_activities: Vec<Enumeration>,
}

impl BareCollection for TimeEntryActivitiesEnvelope {
    type Item = Enumeration;

    fn into_items(self) -> Vec<Enumeration> {
        self.time_entry_activities
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // Inline fixture: see tests/fixtures/README.md for the policy that
    // applies to models with a real API method.
    const JSON: &str = r#"{"id": 1, "name": "Normal", "is_default": true, "active": true}"#;

    #[test]
    fn round_trips() {
        let value: Enumeration = serde_json::from_str(JSON).expect("should parse");
        assert_eq!(value.name, "Normal");
        assert_eq!(value.is_default, Some(true));
    }

    const ISSUE_PRIORITIES_FIXTURE_6_1: &str =
        include_str!("../../tests/fixtures/issue_priority_6_1.json");
    const ISSUE_PRIORITIES_FIXTURE_7_0: &str =
        include_str!("../../tests/fixtures/issue_priority_7_0.json");

    #[test]
    fn issue_priorities_envelope_round_trips_against_6_1_fixture() {
        let env: IssuePrioritiesEnvelope =
            serde_json::from_str(ISSUE_PRIORITIES_FIXTURE_6_1).expect("6.1 fixture should parse");
        assert_eq!(env.issue_priorities.len(), 2);
        assert_eq!(env.issue_priorities.first().unwrap().name, "Low");
    }

    #[test]
    fn issue_priorities_envelope_round_trips_against_7_0_fixture() {
        let env: IssuePrioritiesEnvelope =
            serde_json::from_str(ISSUE_PRIORITIES_FIXTURE_7_0).expect("7.0 fixture should parse");
        assert_eq!(env.issue_priorities.len(), 2);
        assert_eq!(env.issue_priorities.get(1).unwrap().is_default, Some(true));
    }

    const TIME_ENTRY_ACTIVITIES_JSON: &str = r#"{"time_entry_activities": [
        {"id": 8, "name": "Design", "is_default": false, "active": true},
        {"id": 9, "name": "Development", "is_default": true, "active": true}
    ]}"#;

    #[test]
    fn time_entry_activities_envelope_round_trips() {
        let env: TimeEntryActivitiesEnvelope =
            serde_json::from_str(TIME_ENTRY_ACTIVITIES_JSON).expect("should parse");
        assert_eq!(env.time_entry_activities.len(), 2);
        assert_eq!(
            env.time_entry_activities.get(1).unwrap().name,
            "Development"
        );
        assert_eq!(
            env.time_entry_activities.get(1).unwrap().is_default,
            Some(true)
        );
    }
}

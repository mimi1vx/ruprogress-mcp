//! `GET /trackers.json` — no pagination envelope.

use serde::Deserialize;

use super::{BareCollection, IdName};

/// A Redmine tracker (Bug, Feature, Support, ...).
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct Tracker {
    /// The tracker id.
    pub id: u64,
    /// The tracker's display name.
    pub name: String,
    /// The tracker's description.
    #[serde(default)]
    pub description: Option<String>,
    /// The status a new issue of this tracker starts in.
    #[serde(default)]
    pub default_status: Option<IdName>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TrackersEnvelope {
    trackers: Vec<Tracker>,
}

impl BareCollection for TrackersEnvelope {
    type Item = Tracker;

    fn into_items(self) -> Vec<Tracker> {
        self.trackers
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const FIXTURE_6_1: &str = include_str!("../../tests/fixtures/tracker_6_1.json");
    const FIXTURE_7_0: &str = include_str!("../../tests/fixtures/tracker_7_0.json");

    #[test]
    fn round_trips_against_6_1_fixture() {
        let env: TrackersEnvelope =
            serde_json::from_str(FIXTURE_6_1).expect("6.1 fixture should parse");
        assert_eq!(env.trackers.len(), 2);
        assert_eq!(env.trackers.first().unwrap().name, "Bug");
        assert!(env.trackers.get(1).unwrap().default_status.is_none());
    }

    #[test]
    fn round_trips_against_7_0_fixture() {
        let env: TrackersEnvelope =
            serde_json::from_str(FIXTURE_7_0).expect("7.0 fixture should parse");
        assert_eq!(env.trackers.len(), 2);
        assert!(env.trackers.get(1).unwrap().default_status.is_some());
    }

    #[test]
    fn unknown_field_does_not_fail() {
        let json = r#"{"trackers": [{"id": 1, "name": "Bug", "a_future_field": 42}]}"#;
        let env: TrackersEnvelope =
            serde_json::from_str(json).expect("unknown field must not fail parsing");
        assert_eq!(env.trackers.first().unwrap().id, 1);
    }
}

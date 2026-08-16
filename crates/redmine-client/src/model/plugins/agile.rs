//! `RedmineUP` Agile: `GET /issues/{id}/agile_data.json`,
//! `PUT /issues/{id}.json` with a nested `agile_data_attributes`.
//!
//! Synthetic models derived from the reference implementation's handling of
//! this plugin, not a live capture — `RedmineUP` Agile is commercial. See
//! `tests/fixtures/README.md`'s plugin fixtures section.

use serde::{Deserialize, Serialize};

/// One issue's agile row. `story_points` is documented by the plugin as a
/// non-negative integer; a Redmine instance configured for fractional
/// points would fail to deserialize here with a `Decode` error naming the
/// field — the correct, loud failure rather than silently truncating.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgileData {
    /// The row's own id. Must be carried forward on a write — see
    /// [`AgileDataAttributes`]'s doc comment on the replace-vs-merge trap.
    #[serde(default)]
    pub id: Option<u64>,
    /// Story points assigned to the issue, if any.
    #[serde(default)]
    pub story_points: Option<u32>,
    /// The sprint the issue is assigned to, if any.
    #[serde(default)]
    pub agile_sprint_id: Option<u64>,
    /// Wire name is `position`; MCP callers see it as `agile_position` (the
    /// tool layer's rename).
    #[serde(default)]
    pub position: Option<u32>,
}

/// `GET /issues/{id}/agile_data.json` responds `{"agile_data": {...}}` or
/// `{"agile_data": null}` — both handled by the inner `Option`. A `404`
/// (no row at all) is mapped to `Ok(None)` by
/// [`crate::client::Scoped::get_agile_data`], one layer up.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct AgileDataEnvelope {
    /// `None` when the key is `null` or absent.
    #[serde(default)]
    pub agile_data: Option<AgileData>,
}

/// Payload for the `agile_data_attributes` key nested in
/// `PUT /issues/{id}.json`.
///
/// **Replace, not merge.** The plugin declares
/// `accepts_nested_attributes_for :agile_data` without `update_only: true`:
/// omitting the row's `id` here does not mean "leave unset fields alone",
/// it means "create/replace the row", which nulls every field this struct
/// does not carry. A write must always be preceded by a read, and use
/// [`Self::merge_over`] to carry the current row's `id` and every non-null
/// value forward before overlaying the caller's requested change.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AgileDataAttributes {
    /// The existing row's id, carried forward by [`Self::merge_over`].
    /// Omitted only when no row exists yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    /// `Some(None)` sends an explicit `null` (clears the field); `None`
    /// omits the key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub story_points: Option<Option<u32>>,
    /// `0` is the plugin's own sentinel for "remove from its sprint" — sent
    /// literally, not converted to an omitted key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agile_sprint_id: Option<u64>,
    /// Position within its sprint/board.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
}

/// The caller's requested agile change, already resolved to this crate's
/// clearing conventions (the tool layer's own JSON Schema/deserializer
/// concerns do not belong here): `story_points: Some(None)` clears,
/// `Some(Some(n))` sets, `None` leaves alone; `agile_sprint_id: Some(0)`
/// clears, `Some(n)` sets, `None` leaves alone.
#[derive(Debug, Clone, Copy, Default)]
pub struct AgileChange {
    /// `Some(None)` clears, `Some(Some(n))` sets, `None` leaves unchanged.
    pub story_points: Option<Option<u32>>,
    /// `Some(0)` clears (removes from the sprint), `Some(n)` sets, `None`
    /// leaves unchanged.
    pub agile_sprint_id: Option<u64>,
    /// `Some(n)` sets, `None` leaves unchanged.
    pub position: Option<u32>,
}

impl AgileDataAttributes {
    /// Implements the read-modify-write rule documented on this type: carry
    /// `current`'s `id` and every non-null value forward, then let
    /// `requested` win field-by-field, including explicit clears.
    #[must_use]
    pub fn merge_over(current: Option<&AgileData>, requested: &AgileChange) -> Self {
        let mut out = Self {
            id: current.and_then(|c| c.id),
            story_points: current.and_then(|c| c.story_points).map(Some),
            agile_sprint_id: current.and_then(|c| c.agile_sprint_id),
            position: current.and_then(|c| c.position),
        };
        if let Some(story_points) = requested.story_points {
            out.story_points = Some(story_points);
        }
        if let Some(sprint_id) = requested.agile_sprint_id {
            out.agile_sprint_id = Some(sprint_id);
        }
        if let Some(position) = requested.position {
            out.position = Some(position);
        }
        out
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct AgileIssueUpdateEnvelope<'a> {
    pub issue: AgileIssueUpdate<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgileIssueUpdate<'a> {
    pub agile_data_attributes: &'a AgileDataAttributes,
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
    fn envelope_parses_a_populated_row() {
        let json =
            r#"{"agile_data": {"id": 9, "story_points": 8, "agile_sprint_id": 3, "position": 2}}"#;
        let env: AgileDataEnvelope = serde_json::from_str(json).expect("should parse");
        let row = env.agile_data.expect("row should be present");
        assert_eq!(row.id, Some(9));
        assert_eq!(row.story_points, Some(8));
    }

    #[test]
    fn envelope_parses_a_null_row() {
        let json = r#"{"agile_data": null}"#;
        let env: AgileDataEnvelope = serde_json::from_str(json).expect("should parse");
        assert!(env.agile_data.is_none());
    }

    #[test]
    fn merge_over_with_no_current_row_sends_only_the_requested_fields() {
        let attrs = AgileDataAttributes::merge_over(
            None,
            &AgileChange {
                agile_sprint_id: Some(117),
                ..Default::default()
            },
        );
        let value = serde_json::to_value(&attrs).unwrap();
        assert_eq!(value, serde_json::json!({"agile_sprint_id": 117}));
    }

    /// The load-bearing case: a partial update must not null the fields it
    /// did not name.
    #[test]
    fn merge_over_preserves_every_field_the_request_did_not_touch() {
        let current = AgileData {
            id: Some(9),
            story_points: Some(8),
            agile_sprint_id: Some(3),
            position: Some(2),
        };
        let attrs = AgileDataAttributes::merge_over(
            Some(&current),
            &AgileChange {
                agile_sprint_id: Some(7),
                ..Default::default()
            },
        );
        let value = serde_json::to_value(&attrs).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"id": 9, "story_points": 8, "agile_sprint_id": 7, "position": 2})
        );
    }

    #[test]
    fn merge_over_sends_an_explicit_null_to_clear_story_points() {
        let current = AgileData {
            id: Some(9),
            story_points: Some(8),
            agile_sprint_id: None,
            position: None,
        };
        let attrs = AgileDataAttributes::merge_over(
            Some(&current),
            &AgileChange {
                story_points: Some(None),
                ..Default::default()
            },
        );
        let value = serde_json::to_value(&attrs).unwrap();
        assert_eq!(value, serde_json::json!({"id": 9, "story_points": null}));
    }

    #[test]
    fn merge_over_sends_the_zero_sentinel_to_clear_the_sprint() {
        let current = AgileData {
            id: Some(9),
            story_points: None,
            agile_sprint_id: Some(3),
            position: None,
        };
        let attrs = AgileDataAttributes::merge_over(
            Some(&current),
            &AgileChange {
                agile_sprint_id: Some(0),
                ..Default::default()
            },
        );
        let value = serde_json::to_value(&attrs).unwrap();
        assert_eq!(value, serde_json::json!({"id": 9, "agile_sprint_id": 0}));
    }

    #[test]
    fn envelope_serializes_the_nested_shape() {
        let attrs = AgileDataAttributes {
            id: Some(9),
            story_points: Some(Some(8)),
            ..Default::default()
        };
        let value = serde_json::to_value(AgileIssueUpdateEnvelope {
            issue: AgileIssueUpdate {
                agile_data_attributes: &attrs,
            },
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"issue": {"agile_data_attributes": {"id": 9, "story_points": 8}}})
        );
    }
}

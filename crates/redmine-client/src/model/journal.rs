//! Journal entries (issue notes and field-change history), embedded in an
//! [`crate::model::issue::Issue`] via `include=journals`. No standalone
//! endpoint exists — journals are only ever read as part of an issue.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{IdName, permissive_datetime};

/// One field change recorded on a [`Journal`]. Redmine calls the wire field
/// `name` the `prop_key` internally; `property` distinguishes an attribute
/// change (`"attr"`) from a custom-field change (`"cf"`) from an attachment
/// event (`"attachment"`).
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct JournalDetail {
    /// `"attr"`, `"cf"`, `"attachment"`, or `"relation"`.
    pub property: String,
    /// The changed field's key (e.g. `"status_id"`, or a custom field id).
    pub name: String,
    /// The value before the change, if any.
    #[serde(default)]
    pub old_value: Option<String>,
    /// The value after the change, if any.
    #[serde(default)]
    pub new_value: Option<String>,
}

/// A journal entry: a note, a field-change record, or both.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct Journal {
    /// The journal id.
    pub id: u64,
    /// Who created the entry.
    #[serde(default)]
    pub user: Option<IdName>,
    /// The note text, if any. Empty or absent for a field-change-only entry.
    #[serde(default)]
    pub notes: Option<String>,
    /// When the entry was created.
    #[serde(deserialize_with = "permissive_datetime")]
    pub created_on: DateTime<Utc>,
    /// `true` when this entry is a private note, visible only to users with
    /// the "View private notes" permission. Redmine's own visibility
    /// filtering (`Issue#visible_journals_with_index`) already excludes
    /// journals the requesting credential cannot see — this flag is a
    /// property of an already-visible journal, not something callers must
    /// re-derive.
    #[serde(default)]
    pub private_notes: Option<bool>,
    /// Field changes recorded alongside (or instead of) a note.
    #[serde(default)]
    pub details: Option<Vec<JournalDetail>>,
}

/// Payload for `PUT /journals/{id}.json`. `notes` requires
/// `edit_issue_notes` (or `edit_own_issue_notes` for the note's own author);
/// `private_notes` requires `set_notes_private` — Redmine enforces both
/// server-side (403), not this client.
#[derive(Debug, Clone, Default, Serialize)]
pub struct JournalUpdate {
    /// New note text. An empty string clears it. `None` leaves it
    /// unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Toggle the private-note flag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_notes: Option<bool>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JournalUpdateEnvelope<'a> {
    pub journal: &'a JournalUpdate,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_note_with_details() {
        let json = r#"{
            "id": 42,
            "user": {"id": 1, "name": "Alice"},
            "notes": "Fixed it",
            "created_on": "2026-01-01T00:00:00Z",
            "private_notes": true,
            "details": [
                {"property": "attr", "name": "status_id", "old_value": "1", "new_value": "3"}
            ]
        }"#;
        let journal: Journal = serde_json::from_str(json).expect("should parse");
        assert_eq!(journal.id, 42);
        assert_eq!(journal.private_notes, Some(true));
        assert_eq!(journal.details.expect("details").len(), 1);
    }

    #[test]
    fn round_trips_a_field_change_only_entry_with_no_notes() {
        let json = r#"{
            "id": 43,
            "created_on": "2026-01-01T00:00:00Z",
            "details": []
        }"#;
        let journal: Journal = serde_json::from_str(json).expect("should parse");
        assert_eq!(journal.notes, None);
        assert!(journal.user.is_none());
    }

    #[test]
    fn journal_update_serializes_only_set_fields() {
        let patch = JournalUpdate {
            notes: Some(String::new()),
            private_notes: None,
        };
        let value = serde_json::to_value(JournalUpdateEnvelope { journal: &patch }).unwrap();
        let obj = value
            .get("journal")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert_eq!(obj["notes"], "");
        assert!(!obj.contains_key("private_notes"));
    }

    #[test]
    fn unknown_field_does_not_fail_parsing() {
        let json = r#"{
            "id": 1, "created_on": "2026-01-01T00:00:00Z",
            "updated_by": {"id": 1, "name": "Alice"}
        }"#;
        let journal: Journal = serde_json::from_str(json).expect("should parse");
        assert_eq!(journal.id, 1);
    }
}

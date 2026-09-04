//! Typed Redmine API models.
//!
//! `deny_unknown_fields` is deliberately **off** everywhere: Redmine adds
//! fields across versions and plugins inject their own. All public response
//! structs are `#[non_exhaustive]`; write payloads (`*Create`/`*Update`) are
//! separate, constructible types.

pub mod attachment;
pub mod custom_field;
pub mod enumeration;
pub mod introspection;
pub mod issue;
pub mod issue_category;
pub mod issue_status;
pub mod journal;
pub mod membership;
pub mod oauth_token;
pub mod plugins;
pub mod project;
pub mod query;
pub mod relation;
pub mod role;
pub mod search;
pub mod time_entry;
pub mod tracker;
pub mod upload;
pub mod user;
pub mod version;
pub mod wiki;

pub use custom_field::CustomField;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One field of a write payload: left alone, cleared, or set to a value.
///
/// Redmine clears a field when it receives an **empty string** for it, and
/// JSON `null` is not a synonym. Against progress.opensuse.org (Redmine 6.x),
/// `{"issue": {"assigned_to_id": null}}` answers `204` and leaves the
/// assignee untouched, while `{"issue": {"assigned_to_id": ""}}` unassigns
/// the issue; `null` happens to clear the dates and the estimate, but a
/// per-field rule is not something a caller should have to know. This type
/// always sends `""`.
///
/// [`Unchanged`](Self::Unchanged) is the [`Default`], so a payload built with
/// `..Default::default()` omits every field it does not name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FieldUpdate<T> {
    /// Leave the field as it is. The key is omitted from the payload.
    #[default]
    Unchanged,
    /// Clear the field back to unset, by sending an empty string.
    Clear,
    /// Set the field to this value.
    Set(T),
}

impl<T> FieldUpdate<T> {
    /// Whether this leaves the field alone. Every [`FieldUpdate`] field of a
    /// payload struct needs
    /// `#[serde(skip_serializing_if = "FieldUpdate::is_unchanged")]`.
    #[must_use]
    pub const fn is_unchanged(&self) -> bool {
        matches!(*self, Self::Unchanged)
    }
}

impl<T: Copy> FieldUpdate<T> {
    /// `Some(value)` sets the field, `None` leaves it **unchanged** — not
    /// cleared. Clearing has no `Option` spelling on purpose: a caller that
    /// means it says [`FieldUpdate::Clear`].
    ///
    /// `const` (and thus the `T: Copy` bound) because dropping a generic
    /// `Option<T>` is not const-evaluable otherwise; every current
    /// `FieldUpdate<T>` is a `Copy` type.
    #[must_use]
    pub const fn from_option(value: Option<T>) -> Self {
        match value {
            Some(v) => Self::Set(v),
            None => Self::Unchanged,
        }
    }
}

impl<T: Serialize> Serialize for FieldUpdate<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            // Only reachable through a field that forgot its
            // `skip_serializing_if`. Emitting `null` there would put the one
            // value this type exists to keep off the wire back on it, and
            // Redmine would answer 204 having done nothing.
            Self::Unchanged => Err(serde::ser::Error::custom(
                "FieldUpdate::Unchanged serialized without skip_serializing_if",
            )),
            Self::Clear => serializer.serialize_str(""),
            Self::Set(value) => value.serialize(serializer),
        }
    }
}

/// The ubiquitous Redmine `{ "id": 3, "name": "Bug" }` shape.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct IdName {
    /// The referenced resource's id.
    pub id: u64,
    /// The referenced resource's display name.
    pub name: String,
}

/// The `{ "id": 100 }` shape Redmine uses for `Issue.parent` —
/// `issues/show.api.rsb` renders `api.parent(:id => @issue.parent_id)` with
/// no accompanying name, unlike every other `IdName`-shaped association on
/// an issue. Do not conflate with [`IdName`]: deserializing a bare `{"id":
/// N}` into a struct requiring a non-optional `name` field is a decode
/// error, not a graceful fallback.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct IdOnly {
    /// The referenced resource's id.
    pub id: u64,
}

/// A collection response's pagination envelope, implemented per resource
/// (the array's key name — `"issues"`, `"projects"`, ... — varies).
pub(crate) trait Collection: serde::de::DeserializeOwned {
    /// The element type of the collection.
    type Item;
    /// Total number of items across all pages, as reported by Redmine.
    fn total_count(&self) -> u64;
    /// Offset of the first item in this page.
    fn offset(&self) -> u64;
    /// Page size used for this response.
    fn limit(&self) -> u32;
    /// Consume the envelope, yielding just the items.
    fn into_items(self) -> Vec<Self::Item>;
}

/// A collection response with **no** pagination envelope at all — just
/// `{"<key>": [...]}`, no `total_count`/`offset`/`limit`. Kept as a distinct
/// trait from [`Collection`] rather than making the latter's pagination
/// fields `Option`: the difference between a paginated and an un-paginated
/// endpoint is load-bearing and belongs in the type system, not a runtime
/// check that can be gotten wrong per call site.
pub(crate) trait BareCollection: serde::de::DeserializeOwned {
    /// The element type of the collection.
    type Item;
    /// Consume the envelope, yielding just the items.
    fn into_items(self) -> Vec<Self::Item>;
}

/// Parse a Redmine timestamp that may or may not carry a UTC suffix.
/// Some configurations emit `"2025-01-15T10:00:00"` (naive, assumed UTC)
/// instead of RFC 3339's `"...Z"`.
fn parse_permissive_datetime(s: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&chrono::Utc));
    }
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .map(|naive| naive.and_utc())
        .map_err(|e| format!("{s:?} is neither RFC 3339 nor naive `%Y-%m-%dT%H:%M:%S`: {e}"))
}

/// `#[serde(deserialize_with = "permissive_datetime")]` for a required field.
pub(crate) fn permissive_datetime<'de, D>(
    deserializer: D,
) -> Result<chrono::DateTime<chrono::Utc>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse_permissive_datetime(&s).map_err(serde::de::Error::custom)
}

/// `#[serde(deserialize_with = "permissive_datetime_opt", default)]` for an
/// optional field.
pub(crate) fn permissive_datetime_opt<'de, D>(
    deserializer: D,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<String>::deserialize(deserializer)? {
        None => Ok(None),
        Some(s) => parse_permissive_datetime(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn permissive_datetime_accepts_rfc3339_and_naive() {
        assert_eq!(
            parse_permissive_datetime("2026-08-05T10:00:00Z").expect("rfc3339"),
            "2026-08-05T10:00:00Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap()
        );
        assert_eq!(
            parse_permissive_datetime("2026-08-05T10:00:00").expect("naive"),
            "2026-08-05T10:00:00Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap()
        );
    }

    #[test]
    fn field_update_unchanged_refuses_to_serialize() {
        // A field that forgot its `skip_serializing_if` fails here rather
        // than sending a `null` Redmine would accept and ignore.
        let error = serde_json::to_value(FieldUpdate::<u64>::Unchanged)
            .expect_err("Unchanged must not serialize");
        assert!(
            error.to_string().contains("skip_serializing_if"),
            "unexpected error: {error}"
        );
    }
}

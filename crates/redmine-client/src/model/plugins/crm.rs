//! `RedmineUP` CRM: `GET/POST /contacts.json`, `GET/PUT/DELETE
//! /contacts/{id}.json`, `POST /contacts/{id}/projects.json`, `DELETE
//! /contacts/{id}/projects/{pid}.json`.
//!
//! Synthetic models derived from the reference implementation's handling of
//! this plugin, not a live capture — CRM is commercial. See
//! `tests/fixtures/README.md`'s plugin fixtures section.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::ProjectIdent;
use crate::model::{Collection, IdName, permissive_datetime_opt};

/// Filter and sort parameters for `GET /contacts.json`.
#[derive(Debug, Default, Clone)]
pub struct ContactQuery {
    /// Restrict to contacts associated with one project.
    pub project_id: Option<ProjectIdent>,
    /// Free-text search (matches name/company/email).
    pub search: Option<String>,
    /// Comma-separated tag filter, passed through as the reference sends it
    /// (R10) — this is a filter expression the plugin parses, not a set of
    /// values this client is writing.
    pub tags: Option<String>,
    /// Restrict to contacts assigned to this user id.
    pub assigned_to_id: Option<u64>,
}

impl ContactQuery {
    /// Convert to the query-parameter map sent on the wire.
    #[must_use]
    pub fn to_query(&self) -> crate::client::Query {
        let mut q = crate::client::Query::default();
        if let Some(project_id) = &self.project_id {
            q.insert("project_id", project_id.to_string());
        }
        if let Some(search) = &self.search {
            q.insert("search", search.clone());
        }
        if let Some(tags) = &self.tags {
            q.insert("tags", tags.clone());
        }
        if let Some(assigned_to_id) = self.assigned_to_id {
            q.insert("assigned_to_id", assigned_to_id.to_string());
        }
        q
    }
}

/// A contact's postal address, as `RedmineUP` CRM nests it in a response.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContactAddress {
    /// Street address, line 1.
    #[serde(default)]
    pub street1: Option<String>,
    /// Street address, line 2.
    #[serde(default)]
    pub street2: Option<String>,
    /// City.
    #[serde(default)]
    pub city: Option<String>,
    /// State/region.
    #[serde(default)]
    pub region: Option<String>,
    /// Country.
    #[serde(default)]
    pub country: Option<String>,
    /// Postal code.
    #[serde(default)]
    pub postcode: Option<String>,
}

/// A contact's postal address, on the write side. Separate from
/// [`ContactAddress`] (which is `#[non_exhaustive]`, response-only) so
/// callers outside this crate can actually construct one; the field names
/// happen to match, but that is incidental, not a modelling reason to
/// couple the two. Sent as `address_attributes` (Rails nested-attributes
/// convention, same shape as the Agile plugin's `agile_data_attributes`),
/// unverified against a live instance (P5). Only the fields set here are
/// sent, so a partial address update does not blank out the rest —
/// unlike Agile's replace-the-whole-row footgun, nothing in the reference
/// implementation suggests this plugin merges nested attributes any
/// differently from an ordinary Rails `accepts_nested_attributes_for`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ContactAddressWrite {
    /// Street address, line 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub street1: Option<String>,
    /// Street address, line 2.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub street2: Option<String>,
    /// City.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// State/region.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Country.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// Postal code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postcode: Option<String>,
}

/// A `RedmineUP` CRM contact. Every field but `id` is `#[serde(default)]`:
/// the reference implementation has observed plugin versions that omit any
/// of them.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct Contact {
    /// The contact id.
    #[serde(default)]
    pub id: u64,
    /// Given name.
    #[serde(default)]
    pub first_name: Option<String>,
    /// Family name.
    #[serde(default)]
    pub last_name: Option<String>,
    /// Middle name.
    #[serde(default)]
    pub middle_name: Option<String>,
    /// Company/organization name.
    #[serde(default)]
    pub company: Option<String>,
    /// Job title.
    #[serde(default)]
    pub job_title: Option<String>,
    /// Phone number.
    #[serde(default)]
    pub phone: Option<String>,
    /// Email address.
    #[serde(default)]
    pub email: Option<String>,
    /// Website URL.
    #[serde(default)]
    pub website: Option<String>,
    /// Skype username.
    #[serde(default)]
    pub skype_name: Option<String>,
    /// `YYYY-MM-DD`, as Redmine sends dates (not a full timestamp).
    #[serde(default)]
    pub birthday: Option<String>,
    /// Free-form notes about the contact.
    #[serde(default)]
    pub background: Option<String>,
    /// Postal address.
    #[serde(default)]
    pub address: Option<ContactAddress>,
    /// `true` if this contact record represents a company rather than a
    /// person.
    #[serde(default)]
    pub is_company: Option<bool>,
    /// Tags attached to the contact.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// `0` = Project, `1` = Public, `2` = Private.
    #[serde(default)]
    pub visibility: Option<u8>,
    /// The user this contact is assigned to.
    #[serde(default)]
    pub assigned_to: Option<IdName>,
    /// When the contact was created.
    #[serde(default, deserialize_with = "permissive_datetime_opt")]
    pub created_on: Option<DateTime<Utc>>,
    /// When the contact was last updated.
    #[serde(default, deserialize_with = "permissive_datetime_opt")]
    pub updated_on: Option<DateTime<Utc>>,
}

/// Payload for `POST /contacts.json` and `PUT /contacts/{id}.json`. Every
/// field optional and shared between create and update: only fields set
/// here are sent.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ContactWrite {
    /// Given name. Required by Redmine on create.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    /// Family name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    /// Middle name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub middle_name: Option<String>,
    /// Company/organization name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    /// Job title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_title: Option<String>,
    /// Phone number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    /// Email address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Website URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    /// Skype username.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skype_name: Option<String>,
    /// `YYYY-MM-DD`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub birthday: Option<String>,
    /// Free-form notes about the contact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    /// Postal address; only the sub-fields set here are sent (see
    /// [`ContactAddressWrite`]'s doc comment).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address_attributes: Option<ContactAddressWrite>,
    /// `true` to mark this contact as a company rather than a person.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_company: Option<bool>,
    /// `0` = Project, `1` = Public, `2` = Private.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<u8>,
    /// The user to assign this contact to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_to_id: Option<u64>,
    /// The project to associate this contact with. Required by Redmine on
    /// create.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectIdent>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ContactWriteEnvelope<'a> {
    pub contact: &'a ContactWrite,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ContactEnvelope {
    pub contact: Contact,
}

/// `GET /contacts.json`, real pagination (`total_count`/`offset`/`limit`
/// required, not `Option` — R3): a plugin version that omits them is a loud
/// `Decode` error, not a silently-presented first page.
#[derive(Debug, Deserialize)]
pub(crate) struct ContactsEnvelope {
    contacts: Vec<Contact>,
    total_count: u64,
    offset: u64,
    limit: u32,
}

impl Collection for ContactsEnvelope {
    type Item = Contact;

    fn total_count(&self) -> u64 {
        self.total_count
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn limit(&self) -> u32 {
        self.limit
    }

    fn into_items(self) -> Vec<Contact> {
        self.contacts
    }
}

/// `include=` values accepted by `GET /contacts/{id}.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactInclude {
    /// The contact's notes.
    Notes,
    /// Associated deals.
    Deals,
    /// Related contacts.
    Contacts,
}

impl ContactInclude {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Notes => "notes",
            Self::Deals => "deals",
            Self::Contacts => "contacts",
        }
    }
}

/// Build the `include=a,b,c` query value for a slice of includes.
pub(crate) fn includes_to_query_value(includes: &[ContactInclude]) -> Option<String> {
    if includes.is_empty() {
        return None;
    }
    Some(
        includes
            .iter()
            .map(|i| i.as_str())
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// Payload for `POST /contacts/{id}/projects.json`.
#[derive(Debug, Serialize)]
pub(crate) struct ProjectAssocWrite {
    pub project_id: ProjectIdent,
}

#[derive(Debug, Serialize)]
pub(crate) struct ProjectAssocEnvelope<'a> {
    pub project: &'a ProjectAssocWrite,
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
    fn contact_missing_every_optional_field_still_parses() {
        let json = r#"{"id": 5}"#;
        let contact: Contact = serde_json::from_str(json).expect("should parse");
        assert_eq!(contact.id, 5);
        assert_eq!(contact.first_name, None);
        assert!(contact.address.is_none());
    }

    #[test]
    fn contact_with_an_empty_address_object_parses() {
        let json = r#"{"id": 1, "address": {}}"#;
        let contact: Contact = serde_json::from_str(json).expect("should parse");
        assert!(contact.address.is_some());
        assert_eq!(contact.address.unwrap().street1, None);
    }

    #[test]
    fn contact_with_every_field_parses() {
        let json = r#"{
            "id": 1, "first_name": "Ada", "last_name": "Lovelace", "company": "Analytical",
            "email": "ada@example.test", "phone": "+1-555-0100",
            "address": {"street1": "1 Main St", "city": "London", "country": "UK"},
            "is_company": false, "tags": ["vip"], "visibility": 1,
            "assigned_to": {"id": 3, "name": "Bob"},
            "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-02T00:00:00Z"
        }"#;
        let contact: Contact = serde_json::from_str(json).expect("should parse");
        assert_eq!(contact.first_name.as_deref(), Some("Ada"));
        assert_eq!(contact.assigned_to.unwrap().name, "Bob");
    }

    #[test]
    fn contacts_envelope_missing_total_count_is_a_decode_error() {
        let json = r#"{"contacts": []}"#;
        assert!(serde_json::from_str::<ContactsEnvelope>(json).is_err());
    }

    #[test]
    fn write_serializes_only_set_fields() {
        let write = ContactWrite {
            first_name: Some("Ada".to_string()),
            ..ContactWrite::default()
        };
        let value = serde_json::to_value(ContactWriteEnvelope { contact: &write }).unwrap();
        let obj = value
            .get("contact")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert_eq!(obj["first_name"], "Ada");
        assert_eq!(obj.len(), 1);
    }

    #[test]
    fn write_serializes_address_attributes_with_only_the_set_fields() {
        let write = ContactWrite {
            address_attributes: Some(ContactAddressWrite {
                city: Some("London".to_string()),
                ..ContactAddressWrite::default()
            }),
            ..ContactWrite::default()
        };
        let value = serde_json::to_value(ContactWriteEnvelope { contact: &write }).unwrap();
        assert_eq!(value["contact"]["address_attributes"]["city"], "London");
        let nested = value["contact"]["address_attributes"].as_object().unwrap();
        assert_eq!(nested.len(), 1, "only city was set: {nested:?}");
    }

    #[test]
    fn includes_to_query_value_joins_with_commas() {
        assert_eq!(
            includes_to_query_value(&[ContactInclude::Notes, ContactInclude::Deals]),
            Some("notes,deals".to_string())
        );
        assert_eq!(includes_to_query_value(&[]), None);
    }
}

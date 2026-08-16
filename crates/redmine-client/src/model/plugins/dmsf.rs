//! DMSF (`redmine_dmsf`, GPL v2): `GET /projects/{pid}/dmsf.json`,
//! `GET /dmsf_files/{id}.json`, `POST /uploads.json` +
//! `POST /projects/{pid}/dmsf/commit.json`,
//! `POST /dmsf/files/{id}/revision/create.json`.
//!
//! Synthetic models derived from the reference implementation's handling of
//! this plugin (whose module docstring cites the plugin's own
//! `dmsf_upload_helper.rb` and `dmsf_files_controller#create_revision`), not
//! a live capture — see `tests/fixtures/README.md`'s plugin fixtures
//! section. Unlike the other three plugin families, `redmine_dmsf` is
//! open-source, so this is the one family that could in principle be
//! verified against a live instance.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use crate::model::custom_field::CustomFieldWrite;
use crate::model::{IdName, permissive_datetime_opt};

/// One DMSF node — a file, folder, or link to either — merged from whichever
/// endpoint produced it: flat on `GET .../dmsf.json` (list), nested inside
/// the latest `dmsf_file_revisions` entry on `GET /dmsf_files/{id}.json`
/// (show, via [`DmsfFileShowEnvelope::into_node`]), or sparse on the commit
/// response (via [`DmsfCommitResponse::into_nodes`]). Every field but `id`
/// is `#[serde(default)]`/optional: no single source populates all of them.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct DmsfNode {
    /// The node id (a `DmsfFile` id for `type = "file"`).
    #[serde(default)]
    pub id: u64,
    /// `"file"`, `"folder"`, `"file-link"`, or `"folder-link"`.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// The stored filename. Spelled `name` on the wire for files (trap 2 in
    /// the write direction; the list endpoint's own field is `filename`).
    #[serde(default)]
    pub filename: Option<String>,
    /// Display title, distinct from `filename`/`name`.
    #[serde(default)]
    pub title: Option<String>,
    /// The stored filename as the show endpoint's revision spells it. Kept
    /// separate from `filename` since the two endpoints use different key
    /// names for what is, on a file node, the same value.
    #[serde(default)]
    pub name: Option<String>,
    /// Free-text description.
    #[serde(default)]
    pub description: Option<String>,
    /// `"major.minor.patch"`, when the node is a file with at least one
    /// revision.
    #[serde(default)]
    pub version: Option<String>,
    /// Size in bytes, for a file node.
    #[serde(default)]
    pub size: Option<u64>,
    /// MIME type, for a file node.
    #[serde(default)]
    pub content_type: Option<String>,
    /// The containing folder, or `None` for the project root.
    #[serde(default)]
    pub folder_id: Option<u64>,
    /// The owning project.
    #[serde(default)]
    pub project_id: Option<u64>,
    /// Who created (list) or authored the latest revision (show).
    #[serde(default)]
    pub author: Option<IdName>,
    /// When the node (list) or its latest revision (show) was created.
    #[serde(default, deserialize_with = "permissive_datetime_opt")]
    pub created_on: Option<DateTime<Utc>>,
    /// When the node (list) or its latest revision (show) was last updated.
    #[serde(default, deserialize_with = "permissive_datetime_opt")]
    pub updated_on: Option<DateTime<Utc>>,
}

/// `GET /projects/{pid}/dmsf.json` responds with either
/// `{"dmsf": {"dmsf_nodes": [...], "total_count": N}}` or a bare `[...]`
/// array — the two shapes this client accepts (D8). Two more shapes have
/// been observed in the wild (`{"dmsf": [...]}`, a legacy `nodes` key); both
/// are deliberately rejected as a `Decode` error naming the endpoint rather
/// than silently tolerated, so a plugin version emitting one is loud rather
/// than indistinguishable from an empty result. A manual `Deserialize`, not
/// `#[serde(untagged)]` — same rule as [`crate::model::custom_field::CustomFieldValue`].
///
/// `total_count` is carried when the canonical shape provides it; the bare
/// array shape and any canonical response omitting it leave it `None`, so
/// [`crate::client::Scoped::list_dmsf_nodes`] falls back to the fetched
/// item count rather than using [`crate::model::Collection`] (which would
/// require the field unconditionally, per the stricter guarantee the
/// Products/CRM plugins' own index actions give).
#[derive(Debug)]
pub(crate) struct DmsfListEnvelope {
    pub(crate) nodes: Vec<DmsfNode>,
    pub(crate) total_count: Option<u64>,
}

impl<'de> Deserialize<'de> for DmsfListEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let reject = || {
            serde::de::Error::custom(
                "GET .../dmsf.json: expected a dmsf node array or a \
                 {\"dmsf\": {\"dmsf_nodes\": [...]}} envelope, got a different shape",
            )
        };
        match value {
            serde_json::Value::Array(_) => {
                let nodes: Vec<DmsfNode> =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(Self {
                    nodes,
                    total_count: None,
                })
            }
            serde_json::Value::Object(ref map) => match map.get("dmsf") {
                Some(serde_json::Value::Object(inner)) if inner.contains_key("dmsf_nodes") => {
                    #[derive(Deserialize)]
                    struct Inner {
                        #[serde(default)]
                        dmsf_nodes: Vec<DmsfNode>,
                        #[serde(default)]
                        total_count: Option<u64>,
                    }
                    #[derive(Deserialize)]
                    struct Envelope {
                        dmsf: Inner,
                    }
                    let env: Envelope =
                        serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                    Ok(Self {
                        nodes: env.dmsf.dmsf_nodes,
                        total_count: env.dmsf.total_count,
                    })
                }
                _ => Err(reject()),
            },
            _ => Err(reject()),
        }
    }
}

/// One entry of `GET /dmsf_files/{id}.json`'s `dmsf_file_revisions` array —
/// the plugin's own `_at` timestamp spelling (a Rails `ActiveRecord`
/// default), unlike Redmine core's `_on` fields.
#[derive(Debug, Clone, Default, Deserialize)]
struct DmsfFileRevisionRaw {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    version_major: Option<u32>,
    #[serde(default)]
    version_minor: Option<u32>,
    #[serde(default)]
    version_patch: Option<u32>,
    #[serde(default)]
    author: Option<IdName>,
    #[serde(default, deserialize_with = "permissive_datetime_opt")]
    created_at: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "permissive_datetime_opt")]
    updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct DmsfFileShowRaw {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    project_id: Option<u64>,
    #[serde(default)]
    folder_id: Option<u64>,
    #[serde(default)]
    dmsf_file_revisions: Vec<DmsfFileRevisionRaw>,
}

/// `GET /dmsf_files/{id}.json`. Current metadata is nested in the **latest**
/// `dmsf_file_revisions` entry (ascending order, so the last element) —
/// unlike the list endpoint, which reports it flat on the node.
#[derive(Debug, Deserialize)]
pub(crate) struct DmsfFileShowEnvelope {
    dmsf_file: DmsfFileShowRaw,
}

impl DmsfFileShowEnvelope {
    /// Merges the show response's outer id/project/folder with its latest
    /// revision into a [`DmsfNode`] equivalent to what the list endpoint
    /// would report for the same document. `None` when the document carries
    /// **no** revision at all — Redmine's own invariant is that a `DmsfFile`
    /// always has at least one, so a response with none is not a document
    /// this client can make sense of; the tool layer treats that the same as
    /// a 404 (D4), not a decode error.
    pub(crate) fn into_node(self) -> Option<DmsfNode> {
        let mut show = self.dmsf_file;
        let latest = show.dmsf_file_revisions.pop()?;
        let version = (latest.version_major.is_some()
            || latest.version_minor.is_some()
            || latest.version_patch.is_some())
        .then(|| {
            format!(
                "{}.{}.{}",
                latest.version_major.unwrap_or(0),
                latest.version_minor.unwrap_or(0),
                latest.version_patch.unwrap_or(0)
            )
        });
        Some(DmsfNode {
            id: show.id,
            kind: Some("file".to_string()),
            filename: latest.name.clone(),
            title: latest.title,
            name: latest.name,
            description: latest.description,
            version,
            size: latest.size,
            content_type: latest.content_type,
            folder_id: show.folder_id,
            project_id: show.project_id,
            author: latest.author,
            created_on: latest.created_at,
            updated_on: latest.updated_at,
        })
    }
}

/// Payload for `POST /uploads.json` → `POST /projects/{pid}/dmsf/commit.json`'s
/// nested `uploaded_file` object.
#[derive(Debug, Clone, Serialize)]
pub struct DmsfUploadedFile {
    /// The token from [`crate::model::upload::Upload::token`].
    pub token: String,
    /// **Trap 2**: the plugin's upload helper reads `committed_file[:name]`
    /// and assigns it to both `DmsfFile.name` and `DmsfFileRevision.name` —
    /// the key is `name`, not `filename`.
    pub name: String,
    /// **Trap 1**: `create_revision`'s controller calls `.scrub.strip`
    /// unconditionally on the *revision*'s title; commit itself tolerates a
    /// missing one, but every value here still comes pre-defaulted from the
    /// tool layer for consistency between the two write actions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Free-text description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A revision comment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// **Trap 5**: version fields are nested here on commit; a
    /// `create_revision` payload ([`DmsfRevisionWrite`]) has no version
    /// fields at all — see that type's doc comment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_major: Option<u32>,
    /// See [`Self::version_major`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_minor: Option<u32>,
    /// See [`Self::version_major`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_patch: Option<u32>,
    /// **Trap 3**: `custom_field_values`, not `custom_fields`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_field_values: Option<Vec<CustomFieldWrite>>,
}

/// The full request for [`crate::client::Scoped::commit_dmsf_upload`]:
/// `uploaded_file` plus the sibling top-level `folder_id`.
#[derive(Debug, Clone)]
pub struct DmsfCommitRequest {
    /// The uploaded file's metadata and the token that names it.
    pub uploaded_file: DmsfUploadedFile,
    /// The destination folder; `None` commits to the project root.
    pub folder_id: Option<u64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DmsfAttachments<'a> {
    pub(crate) uploaded_file: &'a DmsfUploadedFile,
}

#[derive(Debug, Serialize)]
pub(crate) struct DmsfCommitEnvelope<'a> {
    pub(crate) attachments: DmsfAttachments<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) folder_id: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct DmsfCommitFile {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: Option<String>,
}

/// `POST .../dmsf/commit.json`'s response — **deliberately sparse**
/// (`{id, name}` only per file) by the plugin's own design; every other
/// [`DmsfNode`] field is left `None` rather than treated as missing data.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct DmsfCommitResponse {
    #[serde(default)]
    dmsf_files: Vec<DmsfCommitFile>,
}

impl DmsfCommitResponse {
    pub(crate) fn into_nodes(self) -> Vec<DmsfNode> {
        self.dmsf_files
            .into_iter()
            .map(|f| DmsfNode {
                id: f.id,
                kind: Some("file".to_string()),
                filename: f.name.clone(),
                name: f.name,
                title: None,
                description: None,
                version: None,
                size: None,
                content_type: None,
                folder_id: None,
                project_id: None,
                author: None,
                created_on: None,
                updated_on: None,
            })
            .collect()
    }
}

/// Payload for `POST /dmsf/files/{id}/revision/create.json`'s nested
/// `dmsf_file_revision` object. `title`/`name` are **not** `Option`: trap 1
/// means a missing one 500s the server, so the tool layer always supplies
/// both (pre-fetching current values when the caller omitted them) before
/// this type is ever constructed. No version fields (D6): `commit` and
/// `create_revision` read them from different places (trap 5), and DMSF
/// auto-increments the patch version on every revision regardless.
#[derive(Debug, Clone, Serialize)]
pub struct DmsfRevisionWrite {
    /// Display title. Never omitted (trap 1).
    pub title: String,
    /// Stored filename. Never omitted (trap 1).
    pub name: String,
    /// Free-text description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A revision comment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// **Trap 3**: `custom_field_values`, not `custom_fields`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_field_values: Option<Vec<CustomFieldWrite>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DmsfRevisionEnvelope<'a> {
    pub(crate) dmsf_file_revision: &'a DmsfRevisionWrite,
}

/// A parsed `"X"` / `"X.Y"` / `"X.Y.Z"` version string (D7), each part a
/// non-negative integer, missing parts padded with `0`. Parsed and
/// validated by the tool layer **before** any upload request is sent, so a
/// malformed version never leaves an orphaned upload token behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmsfVersion {
    /// The major version part.
    pub major: u32,
    /// The minor version part; `0` when omitted from the input string.
    pub minor: u32,
    /// The patch version part; `0` when omitted from the input string.
    pub patch: u32,
}

impl FromStr for DmsfVersion {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let reject = || {
            format!(
                "invalid version {s:?}: expected \"X\", \"X.Y\", or \"X.Y.Z\", each part a non-negative integer"
            )
        };
        if s.is_empty() {
            return Err(reject());
        }
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() > 3 {
            return Err(reject());
        }
        let mut nums = [0u32; 3];
        for (slot, part) in nums.iter_mut().zip(parts.iter()) {
            *slot = part.parse::<u32>().map_err(|_| reject())?;
        }
        Ok(Self {
            major: nums[0],
            minor: nums[1],
            patch: nums[2],
        })
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
    fn list_envelope_canonical_shape_parses() {
        let json =
            r#"{"dmsf": {"dmsf_nodes": [{"id": 1, "filename": "report.pdf"}], "total_count": 1}}"#;
        let env: DmsfListEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(env.nodes.len(), 1);
        assert_eq!(env.total_count, Some(1));
    }

    #[test]
    fn list_envelope_bare_array_shape_parses_with_no_total_count() {
        let json = r#"[{"id": 1, "filename": "report.pdf"}]"#;
        let env: DmsfListEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(env.nodes.len(), 1);
        assert_eq!(env.total_count, None);
    }

    #[test]
    fn list_envelope_rejects_the_dmsf_as_bare_array_shape() {
        let json = r#"{"dmsf": [{"id": 1}]}"#;
        let err = serde_json::from_str::<DmsfListEnvelope>(json).unwrap_err();
        assert!(err.to_string().contains("dmsf.json"));
    }

    #[test]
    fn list_envelope_rejects_an_unrecognised_shape() {
        let json = r#"{"unexpected": true}"#;
        let err = serde_json::from_str::<DmsfListEnvelope>(json).unwrap_err();
        assert!(err.to_string().contains("dmsf.json"));
    }

    #[test]
    fn show_envelope_merges_the_latest_revision() {
        let json = r#"{"dmsf_file": {
            "id": 42, "project_id": 1, "folder_id": 3,
            "dmsf_file_revisions": [
                {"title": "Old", "name": "old.pdf", "version_major": 1},
                {"title": "Report", "name": "report.pdf", "description": "Q1",
                 "size": 2048, "content_type": "application/pdf",
                 "version_major": 1, "version_minor": 2, "version_patch": 0,
                 "author": {"id": 1, "name": "Alice"},
                 "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z"}
            ]
        }}"#;
        let env: DmsfFileShowEnvelope = serde_json::from_str(json).expect("should parse");
        let node = env.into_node().expect("should have a latest revision");
        assert_eq!(node.id, 42);
        assert_eq!(node.project_id, Some(1));
        assert_eq!(node.folder_id, Some(3));
        assert_eq!(node.name.as_deref(), Some("report.pdf"));
        assert_eq!(node.title.as_deref(), Some("Report"));
        assert_eq!(node.description.as_deref(), Some("Q1"));
        assert_eq!(node.version.as_deref(), Some("1.2.0"));
        assert_eq!(node.size, Some(2048));
    }

    #[test]
    fn show_envelope_with_no_revisions_merges_to_none() {
        let json = r#"{"dmsf_file": {"id": 42, "dmsf_file_revisions": []}}"#;
        let env: DmsfFileShowEnvelope = serde_json::from_str(json).expect("should parse");
        assert!(env.into_node().is_none());
    }

    /// Proves the show shape and the list shape for the same underlying
    /// document produce field-for-field identical [`DmsfNode`]s on the
    /// fields both endpoints carry.
    #[test]
    fn show_and_list_shapes_merge_to_the_same_node() {
        let show_json = r#"{"dmsf_file": {
            "id": 42, "project_id": 1, "folder_id": 3,
            "dmsf_file_revisions": [{
                "title": "Report", "name": "report.pdf", "description": "Q1",
                "size": 2048, "content_type": "application/pdf",
                "version_major": 1, "version_minor": 2, "version_patch": 0
            }]
        }}"#;
        let show_node: DmsfFileShowEnvelope = serde_json::from_str(show_json).expect("parse show");
        let show_node = show_node.into_node().expect("has a revision");

        let list_json = r#"{"id": 42, "project_id": 1, "folder_id": 3,
            "title": "Report", "name": "report.pdf", "description": "Q1",
            "size": 2048, "content_type": "application/pdf", "version": "1.2.0"}"#;
        let list_node: DmsfNode = serde_json::from_str(list_json).expect("parse list node");

        assert_eq!(show_node.id, list_node.id);
        assert_eq!(show_node.project_id, list_node.project_id);
        assert_eq!(show_node.folder_id, list_node.folder_id);
        assert_eq!(show_node.name, list_node.name);
        assert_eq!(show_node.title, list_node.title);
        assert_eq!(show_node.description, list_node.description);
        assert_eq!(show_node.size, list_node.size);
        assert_eq!(show_node.content_type, list_node.content_type);
        assert_eq!(show_node.version, list_node.version);
    }

    #[test]
    fn commit_response_is_sparse_by_design() {
        let json = r#"{"dmsf_files": [{"id": 7, "name": "report.pdf"}], "total_count": 1}"#;
        let resp: DmsfCommitResponse = serde_json::from_str(json).expect("should parse");
        let nodes = resp.into_nodes();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id, 7);
        assert_eq!(nodes[0].name.as_deref(), Some("report.pdf"));
        assert_eq!(nodes[0].description, None);
        assert_eq!(nodes[0].version, None);
    }

    #[test]
    fn commit_envelope_serializes_the_documented_shape() {
        let uploaded_file = DmsfUploadedFile {
            token: "42.abc".to_string(),
            name: "report.pdf".to_string(),
            title: Some("Report".to_string()),
            description: None,
            comment: None,
            version_major: Some(1),
            version_minor: None,
            version_patch: None,
            custom_field_values: None,
        };
        let envelope = DmsfCommitEnvelope {
            attachments: DmsfAttachments {
                uploaded_file: &uploaded_file,
            },
            folder_id: Some(3),
        };
        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "attachments": {"uploaded_file": {
                    "token": "42.abc", "name": "report.pdf", "title": "Report",
                    "version_major": 1
                }},
                "folder_id": 3
            })
        );
    }

    #[test]
    fn revision_envelope_spells_custom_field_values_not_custom_fields() {
        let write = DmsfRevisionWrite {
            title: "Report".to_string(),
            name: "report.pdf".to_string(),
            description: None,
            comment: None,
            custom_field_values: Some(vec![CustomFieldWrite {
                id: 1,
                value: crate::model::custom_field::CustomFieldValue::Single(Some("x".to_string())),
            }]),
        };
        let envelope = DmsfRevisionEnvelope {
            dmsf_file_revision: &write,
        };
        let value = serde_json::to_value(&envelope).unwrap();
        let inner = value.get("dmsf_file_revision").unwrap();
        assert!(inner.get("custom_field_values").is_some());
        assert!(inner.get("custom_fields").is_none());
    }

    #[test]
    fn version_parses_each_documented_form() {
        let cases: &[(&str, DmsfVersion)] = &[
            (
                "1",
                DmsfVersion {
                    major: 1,
                    minor: 0,
                    patch: 0,
                },
            ),
            (
                "1.2",
                DmsfVersion {
                    major: 1,
                    minor: 2,
                    patch: 0,
                },
            ),
            (
                "1.2.3",
                DmsfVersion {
                    major: 1,
                    minor: 2,
                    patch: 3,
                },
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(DmsfVersion::from_str(input).unwrap(), *expected);
        }
    }

    #[test]
    fn version_rejects_malformed_input() {
        for input in ["1.2.3.4", "1.-2", "a", "", "99999999999"] {
            assert!(
                DmsfVersion::from_str(input).is_err(),
                "expected rejection for {input:?}"
            );
        }
    }
}

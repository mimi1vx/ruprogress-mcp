//! `GET /attachments/{id}`, `GET /projects/{id}/files.json`, and attachments
//! embedded in other resources.
//!
//! **`container_type`/`container_id` are deliberately not modeled.**
//! Verified against `redmine/redmine` upstream source (both the `6.1-stable`
//! branch and `master`, i.e. the 7.0 line) on 2026-08-07:
//! `AttachmentsHelper#render_api_attachment_attributes` — the method behind
//! both `GET /attachments/{id}.json` and every embedded-attachment shape —
//! renders `id`, `filename`, `filesize`, `content_type`, `description`,
//! `content_url`, an optional `thumbnail_url`, `author`, and `created_on`.
//! It never renders the container. Redmine's `Attachment` model *has*
//! `container_type`/`container_id` columns (it is a polymorphic
//! association), but the API never exposes them. As a result `delete_file`
//! cannot implement a container-type scope check from this response and
//! must fall back to unconditionally requiring
//! `confirm_delete_any_attachment`.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{BareCollection, IdName, permissive_datetime};

/// A file attached to an issue, wiki page, project (Files module), or other
/// resource.
///
/// `digest`, `downloads`, and `version` are only ever populated by the
/// Files-module listing (`GET /projects/{id}/files.json`,
/// `files/index.api.rsb`) — `GET /attachments/{id}.json` never renders them,
/// so they are `None` there. `version` is further `None` even from the
/// Files listing when the file is attached directly to the project rather
/// than to one of its versions (`files/index.api.rsb` only emits `version`
/// `if container.is_a?(Version)`).
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct Attachment {
    /// The attachment id.
    pub id: u64,
    /// The original filename.
    pub filename: String,
    /// Size in bytes.
    pub filesize: u64,
    /// The MIME type, if Redmine recorded one.
    #[serde(default)]
    pub content_type: Option<String>,
    /// The uploader-supplied description.
    #[serde(default)]
    pub description: Option<String>,
    /// URL to download the file content.
    pub content_url: String,
    /// Who uploaded the file.
    #[serde(default)]
    pub author: Option<IdName>,
    /// When the file was uploaded.
    #[serde(deserialize_with = "permissive_datetime")]
    pub created_on: DateTime<Utc>,
    /// MD5 digest of the file content. Files-module listing only.
    #[serde(default)]
    pub digest: Option<String>,
    /// Number of times the file has been downloaded. Files-module listing
    /// only.
    #[serde(default)]
    pub downloads: Option<u64>,
    /// The project version this file is attached to, if any. Files-module
    /// listing only, and only when the file's container is a `Version`
    /// rather than the `Project` itself.
    #[serde(default)]
    pub version: Option<IdName>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AttachmentEnvelope {
    pub attachment: Attachment,
}

/// `GET /projects/{id}/files.json` — `{"files": [...]}`, no pagination
/// envelope (`files/index.api.rsb` is a bare `api.array`).
#[derive(Debug, Deserialize)]
pub(crate) struct ProjectFilesEnvelope {
    pub files: Vec<Attachment>,
}

impl BareCollection for ProjectFilesEnvelope {
    type Item = Attachment;

    fn into_items(self) -> Vec<Attachment> {
        self.files
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // Inline fixture: see tests/fixtures/README.md for the policy that
    // applies to models with a real API method.
    const JSON: &str = r#"{"attachment": {
        "id": 1, "filename": "report.pdf", "filesize": 1024,
        "content_url": "https://example.com/attachments/download/1/report.pdf",
        "created_on": "2026-01-01T00:00:00Z"
    }}"#;

    #[test]
    fn round_trips() {
        let env: AttachmentEnvelope = serde_json::from_str(JSON).expect("should parse");
        assert_eq!(env.attachment.filename, "report.pdf");
    }

    #[test]
    fn round_trips_without_digest_downloads_or_version() {
        // GET /attachments/{id}.json shape: none of the Files-module-only
        // fields are present.
        let env: AttachmentEnvelope = serde_json::from_str(JSON).expect("should parse");
        assert!(env.attachment.digest.is_none());
        assert!(env.attachment.downloads.is_none());
        assert!(env.attachment.version.is_none());
    }

    #[test]
    fn project_files_envelope_parses_a_bare_array_with_project_and_version_containers() {
        let json = r#"{"files": [
            {
                "id": 11, "filename": "project-file.txt", "filesize": 10,
                "content_url": "https://example.com/attachments/download/11/project-file.txt",
                "created_on": "2026-01-01T00:00:00Z",
                "digest": "d41d8cd98f00b204e9800998ecf8427e", "downloads": 3
            },
            {
                "id": 12, "filename": "version-file.txt", "filesize": 20,
                "content_url": "https://example.com/attachments/download/12/version-file.txt",
                "created_on": "2026-01-01T00:00:00Z",
                "digest": "d41d8cd98f00b204e9800998ecf8427e", "downloads": 0,
                "version": {"id": 2, "name": "1.0"}
            }
        ]}"#;
        let env: ProjectFilesEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(env.files.len(), 2);
        assert!(env.files.first().unwrap().version.is_none());
        assert_eq!(
            env.files.get(1).unwrap().version.as_ref().unwrap().name,
            "1.0"
        );
    }
}

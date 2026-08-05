//! `GET /attachments/{id}`, and attachments embedded in other resources.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{IdName, permissive_datetime};

/// A file attached to an issue, wiki page, or other resource.
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
}

#[derive(Debug, Deserialize)]
#[allow(
    dead_code,
    reason = "model exists for round-trip tests; no phase-1 API method uses it yet"
)]
pub(crate) struct AttachmentEnvelope {
    pub attachment: Attachment,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // Inline fixture: see tests/fixtures/README.md for the policy that
    // applies to models with a real API method in phase 1's surface.
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
}

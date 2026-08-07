//! `POST /uploads.json` (the two-step attach flow's first step), and
//! `POST /projects/{id}/files.json` (its second step, for the Files
//! module).

use serde::{Deserialize, Serialize};

/// Redmine's answer to `POST /uploads.json`: a token for the just-uploaded,
/// not-yet-attached file, plus its attachment id. `id` is the same id the
/// attachment keeps once it is attached via [`ProjectFileCreate`] — the
/// upload step already creates the `Attachment` row
/// (`AttachmentsController#upload`); attaching it only sets its container.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct Upload {
    /// The attachment id assigned to the uploaded file.
    pub id: u64,
    /// Opaque token, valid for one attach call.
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UploadEnvelope {
    pub upload: Upload,
}

/// Body for `POST /projects/{id}/files.json`'s `file` key: attach an
/// already-uploaded (via [`Upload`]) file to a project, optionally pinning
/// it to one of the project's versions.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProjectFileCreate {
    /// The token from [`Upload::token`].
    pub token: String,
    /// Override the filename recorded at upload time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Override the content type recorded at upload time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Free-text description shown in the Files module.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Attach to this version instead of the project directly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_id: Option<u64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ProjectFileCreateEnvelope<'a> {
    pub file: &'a ProjectFileCreate,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn upload_envelope_round_trips() {
        let json = r#"{"upload": {"id": 42, "token": "42.abcdef0123456789"}}"#;
        let env: UploadEnvelope = serde_json::from_str(json).expect("should parse");
        assert_eq!(env.upload.id, 42);
        assert_eq!(env.upload.token, "42.abcdef0123456789");
    }

    #[test]
    fn project_file_create_omits_unset_optional_fields() {
        let body = ProjectFileCreate {
            token: "42.abcdef0123456789".to_string(),
            ..Default::default()
        };
        let value = serde_json::to_value(ProjectFileCreateEnvelope { file: &body })
            .expect("should serialize");
        assert_eq!(
            value,
            serde_json::json!({"file": {"token": "42.abcdef0123456789"}})
        );
    }

    #[test]
    fn project_file_create_includes_set_optional_fields() {
        let body = ProjectFileCreate {
            token: "42.abcdef0123456789".to_string(),
            filename: Some("report.pdf".to_string()),
            content_type: Some("application/pdf".to_string()),
            description: Some("Q1 report".to_string()),
            version_id: Some(7),
        };
        let value = serde_json::to_value(ProjectFileCreateEnvelope { file: &body })
            .expect("should serialize");
        assert_eq!(
            value,
            serde_json::json!({"file": {
                "token": "42.abcdef0123456789",
                "filename": "report.pdf",
                "content_type": "application/pdf",
                "description": "Q1 report",
                "version_id": 7
            }})
        );
    }
}

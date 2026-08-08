//! File tools. `get_redmine_attachment` is the first: `list_files`/
//! `upload_file`/`delete_file`/`cleanup_attachment_files` land alongside it
//! in this module later.
//!
//! This is the first tool in the codebase that writes to the local
//! filesystem — see `attachments.rs` for the store it writes into.

use futures_util::StreamExt as _;
use redmine_client::AttachmentId;
use redmine_client::model::attachment::Attachment;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt as _;

use crate::attachments::{AttachmentStore, Reservation, StoredFile};
use crate::config::TransportConfig;
use crate::error::to_tool_error;
use crate::server::RedmineMcp;
use crate::tools::output::{self, ErrorCode, err};

// --- get_redmine_attachment ---

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetRedmineAttachmentParams {
    /// The id of the attachment to retrieve.
    pub(crate) attachment_id: u64,
}

/// Exactly one of `uri`/`file_path` is present, selected by which transport
/// is actually running (`config.transport`), never by inspecting headers or
/// the environment at call time.
///
/// `filename`/`content_type` are structured metadata, not free text a
/// project member wrote, so they are **not** boundary-wrapped, unlike
/// `attachment.description` elsewhere in this codebase.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct GetRedmineAttachmentOutput {
    /// Present only when the server is running the HTTP transport
    /// (`uri_type = "http"`): a `/files/{uuid}` URL the caller can fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) uri: Option<String>,
    /// Present only when the server is running the stdio transport
    /// (`uri_type = "file"`): an absolute path on this host, safe to hand to
    /// another local tool that dispatches on the extension.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file_path: Option<String>,
    pub(crate) uri_type: String,
    pub(crate) filename: String,
    pub(crate) content_type: Option<String>,
    pub(crate) size: u64,
    pub(crate) expires_at: chrono::DateTime<chrono::Utc>,
    pub(crate) attachment_id: u64,
}

fn file_too_large(limit: u64, actual: Option<u64>) -> CallToolResult {
    let message = match actual {
        Some(actual) => format!(
            "the attachment is {actual} bytes, larger than the configured per-file limit ({limit} bytes)"
        ),
        None => {
            format!("the attachment is larger than the configured per-file limit ({limit} bytes)")
        }
    };
    err(
        ErrorCode::FileTooLarge,
        message,
        Some(
            "this attachment cannot be downloaded through this server; raise ATTACHMENT_MAX_DOWNLOAD_BYTES if that is expected",
        ),
    )
}

fn local_storage_error(context: &str) -> CallToolResult {
    err(
        ErrorCode::Misconfigured,
        format!("the server could not {context}"),
        Some(
            "this is a server configuration problem the model cannot fix; report it to the operator",
        ),
    )
}

/// Streams `attachment`'s content into `reservation.path` and commits it,
/// enforcing `max_download_bytes` against bytes actually received rather
/// than any header or metadata field — this is what makes a lying
/// `Content-Length` harmless. On any failure the reservation is aborted
/// (its whole UUID directory removed) before the error is returned.
async fn download_and_commit(
    scoped: &redmine_client::Scoped<'_>,
    store: &AttachmentStore,
    attachment: &Attachment,
    reservation: Reservation,
) -> Result<StoredFile, CallToolResult> {
    let (_headers, stream) = match scoped.download_attachment(&attachment.content_url).await {
        Ok(v) => v,
        Err(e) => {
            store.abort(&reservation).await;
            return Err(to_tool_error(e));
        }
    };

    let mut file = match tokio::fs::File::create(&reservation.path).await {
        Ok(f) => f,
        Err(error) => {
            tracing::error!(%error, "failed to create the local file for a downloaded attachment");
            store.abort(&reservation).await;
            return Err(local_storage_error(
                "create a local file to stage this download",
            ));
        }
    };

    let max_download_bytes = store.max_download_bytes();
    let mut stream = std::pin::pin!(stream);
    let mut written: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                drop(file);
                store.abort(&reservation).await;
                return Err(to_tool_error(e));
            }
        };
        written = written.saturating_add(chunk.len() as u64);
        if written > max_download_bytes {
            drop(file);
            store.abort(&reservation).await;
            return Err(file_too_large(max_download_bytes, None));
        }
        if let Err(error) = file.write_all(&chunk).await {
            tracing::error!(%error, "failed to write a downloaded attachment chunk to disk");
            drop(file);
            store.abort(&reservation).await;
            return Err(local_storage_error("write this download to local storage"));
        }
    }
    if let Err(error) = file.flush().await {
        tracing::warn!(%error, "failed to flush a downloaded attachment to disk");
    }
    drop(file);

    Ok(store
        .commit(reservation, attachment.content_type.clone(), written)
        .await)
}

fn get_redmine_attachment_output(
    transport: &TransportConfig,
    stored: &StoredFile,
) -> GetRedmineAttachmentOutput {
    let (uri, file_path, uri_type) = match transport {
        TransportConfig::Stdio => (
            None,
            Some(stored.path.to_string_lossy().into_owned()),
            "file",
        ),
        TransportConfig::Http(http) => {
            let base = &http.public_base;
            let uri = base.join(&format!("files/{}", stored.uuid)).map_or_else(
                |_| format!("{base}files/{}", stored.uuid),
                |u| u.to_string(),
            );
            (Some(uri), None, "http")
        }
    };
    GetRedmineAttachmentOutput {
        uri,
        file_path,
        uri_type: uri_type.to_string(),
        filename: stored.filename.clone(),
        content_type: stored.content_type.clone(),
        size: stored.size,
        expires_at: stored.expires_at,
        attachment_id: stored.attachment_id,
    }
}

#[tool_router(router = files_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// `GET /attachments/{id}.json` for metadata, then streams the file
    /// content into the local attachment store (see `attachments.rs`).
    #[tool(
        description = "Download a Redmine attachment by numeric id, staging it in the server local file store. Use this when the actual file bytes are needed, not just metadata. Returns a /files/{uuid} URL (HTTP transport) or an absolute file_path (stdio) per uri_type; the copy expires after ATTACHMENT_EXPIRES_MINUTES (60 min default), so fetch promptly. attachment_id comes from get_redmine_issue or list_files.",
        input_schema = crate::tools::schema::input::<GetRedmineAttachmentParams>(),
        output_schema = crate::tools::schema::output::<GetRedmineAttachmentOutput>(),
        annotations(read_only_hint = true, idempotent_hint = false, open_world_hint = true),
    )]
    pub(crate) async fn get_redmine_attachment(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<GetRedmineAttachmentParams>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let attachment = match scoped
            .get_attachment(AttachmentId(params.attachment_id))
            .await
        {
            Ok(a) => a,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let store = self.attachments();

        // A cheap pre-check against Redmine's own metadata, skipping a
        // doomed download outright. The real, trustworthy enforcement is the
        // byte counter inside `download_and_commit`.
        if attachment.filesize > store.max_download_bytes() {
            return Ok(file_too_large(
                store.max_download_bytes(),
                Some(attachment.filesize),
            ));
        }

        // Check store capacity before reserving; sweep once if over, then
        // refuse rather than filling the disk.
        if !store.has_room_for(attachment.filesize).await {
            store.sweep_expired().await;
            if !store.has_room_for(attachment.filesize).await {
                return Ok(err(
                    ErrorCode::StoreFull,
                    "the local attachment store is at capacity",
                    Some(
                        "wait for expired entries to be cleaned up, or ask the operator to raise ATTACHMENT_STORE_MAX_BYTES",
                    ),
                ));
            }
        }

        let reservation = match store.reserve(attachment.id, &attachment.filename).await {
            Ok(r) => r,
            Err(error) => {
                tracing::error!(%error, "failed to reserve local storage for a downloaded attachment");
                return Ok(local_storage_error(
                    "allocate local storage for this download",
                ));
            }
        };

        let stored = match download_and_commit(&scoped, &store, &attachment, reservation).await {
            Ok(stored) => stored,
            Err(result) => return Ok(result),
        };

        let output = get_redmine_attachment_output(&self.inner.config.transport, &stored);
        Ok(output::ok(&output, self.output_caps()))
    }
}

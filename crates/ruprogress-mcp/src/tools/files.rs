//! File tools: `get_redmine_attachment`, `list_files`, `delete_file`,
//! `upload_file`, `cleanup_attachment_files`.
//!
//! This is the first tool module in the codebase that writes to the local
//! filesystem — see `attachments.rs` for the store `get_redmine_attachment`
//! writes into and `cleanup_attachment_files` sweeps.

use base64::Engine as _;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures_util::StreamExt as _;
use redmine_client::AttachmentId;
use redmine_client::model::attachment::Attachment;
use redmine_client::model::upload::ProjectFileCreate;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt as _;

use crate::attachments::{AttachmentStore, Reservation, StoredFile};
use crate::config::TransportConfig;
use crate::error::to_tool_error;
use crate::render::Boundary;
use crate::server::RedmineMcp;
use crate::tools::discovery::{ProjectRef, resolve_project_ref};
use crate::tools::issues::{IdNameOut, id_name_out};
use crate::tools::output::{self, ContentUrlRewrite, ErrorCode, err};

/// `upload_file`'s own per-file cap for the `file_path` source: independent
/// of `ATTACHMENT_MAX_DOWNLOAD_BYTES`, which bounds the opposite direction
/// (Redmine → local disk). Matches the vendored reference contract's
/// documented 50 MiB limit; not configurable, since it is a fixed property
/// of this server's `upload_file` implementation, not a deployment choice.
const UPLOAD_FILE_MAX_BYTES: u64 = 50 * 1024 * 1024;

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

/// `context` names whichever field this arity check is guarding — e.g.
/// `"upload_file"` or `"uploads[2]"` (issue uploads reuse this too) — so the
/// message reads correctly regardless of caller.
pub(crate) fn source_required(context: &str) -> CallToolResult {
    err(
        ErrorCode::SourceRequired,
        format!("{context} requires exactly one of content_base64 or file_path"),
        Some("set exactly one source field and retry"),
    )
}

pub(crate) fn unsupported_source() -> CallToolResult {
    err(
        ErrorCode::UnsupportedSource,
        "source_url is not supported by this server; use content_base64 or file_path instead",
        Some(
            "fetch the URL's bytes yourself and pass them via content_base64, or stage the file locally and use file_path",
        ),
    )
}

fn path_not_allowed() -> CallToolResult {
    err(
        ErrorCode::PathNotAllowed,
        "file_path is not inside ATTACHMENTS_DIR or a directory listed in REDMINE_MCP_UPLOAD_FILE_ROOTS, does not exist, or is not a regular file",
        Some(
            "use content_base64 instead, or ask the operator to add this location to REDMINE_MCP_UPLOAD_FILE_ROOTS",
        ),
    )
}

fn upload_path_too_large(actual: u64) -> CallToolResult {
    err(
        ErrorCode::FileTooLarge,
        format!(
            "the file is {actual} bytes, larger than the {UPLOAD_FILE_MAX_BYTES}-byte per-file upload limit"
        ),
        Some(
            "this server cannot upload a file this large; split the content or upload it to Redmine some other way",
        ),
    )
}

fn upload_rejected_as_too_large() -> CallToolResult {
    err(
        ErrorCode::FileTooLarge,
        "Redmine rejected the upload as too large",
        Some(
            "the limit may be Redmine's own attachment_max_size setting, not this server's; ask the operator to check it",
        ),
    )
}

/// Mints one upload token via `POST /uploads.json`, remapping a 413/422 from
/// *this specific step* to `FILE_TOO_LARGE` — a size condition, not a
/// validation one; Redmine's own `attachment_max_size`, not any later
/// attach step's token/field validation. Shared by `upload_file`'s
/// Files-module flow and `create_redmine_issue`/`update_redmine_issue`'s
/// issue-native `uploads[]` — both mint tokens the same way, only
/// what they do with the token afterwards differs.
pub(crate) async fn mint_upload_token(
    scoped: &redmine_client::Scoped<'_>,
    body: Bytes,
    filename: Option<&str>,
) -> Result<redmine_client::model::upload::Upload, CallToolResult> {
    match scoped.create_upload(body, filename, None).await {
        Ok(u) => Ok(u),
        Err(redmine_client::Error::Api { status, .. })
            if status == http::StatusCode::PAYLOAD_TOO_LARGE
                || status == http::StatusCode::UNPROCESSABLE_ENTITY =>
        {
            Err(upload_rejected_as_too_large())
        }
        Err(e) => Err(to_tool_error(e)),
    }
}

/// Validates a `file_path` upload source: reject non-absolute paths,
/// canonicalise, prefix-check against `roots` plus `store_dir`, stat
/// the canonical path to reject non-regular files *before* opening, `open`
/// the canonicalised path, `fstat` the open handle, and on Unix require the
/// fstat'd `(dev, ino)` to match a fresh stat of the canonical path —
/// closing the canonicalise-then-open TOCTOU window.
///
/// The pre-open stat exists for more than defense in depth: `File::open` on
/// a FIFO with no writer blocks the calling thread indefinitely (a device
/// node can have similar surprises), so a hostile or merely misplaced
/// special file inside an allowed root must be rejected by its type before
/// this function ever calls `open`, not only after. A file swapped for a
/// FIFO in the narrow window between this stat and the `open` below would
/// still block — an accepted residual risk, since it requires write access
/// to the exact canonical path of an operator-configured upload root at the
/// exact moment of a request, not something a caller's `file_path` alone
/// can trigger.
///
/// Every failure in this chain — non-absolute, outside every root, missing,
/// not a regular file, a dev/ino mismatch — collapses to the same
/// `PATH_NOT_ALLOWED` error. Distinguishing them would hand a caller an
/// oracle for probing the local filesystem.
pub(crate) async fn read_and_validate_upload_path(
    roots: &[PathBuf],
    store_dir: &Path,
    raw: &str,
) -> Result<(Bytes, Option<String>), CallToolResult> {
    let requested = Path::new(raw);
    if !requested.is_absolute() {
        return Err(path_not_allowed());
    }

    let canonical = tokio::fs::canonicalize(requested)
        .await
        .map_err(|_| path_not_allowed())?;

    let mut allowed = false;
    for root in roots
        .iter()
        .map(PathBuf::as_path)
        .chain(std::iter::once(store_dir))
    {
        if let Ok(canon_root) = tokio::fs::canonicalize(root).await
            && canonical.starts_with(&canon_root)
        {
            allowed = true;
            break;
        }
    }
    if !allowed {
        return Err(path_not_allowed());
    }

    let pre_open_meta = tokio::fs::metadata(&canonical)
        .await
        .map_err(|_| path_not_allowed())?;
    if !pre_open_meta.is_file() {
        return Err(path_not_allowed());
    }
    if pre_open_meta.len() > UPLOAD_FILE_MAX_BYTES {
        return Err(upload_path_too_large(pre_open_meta.len()));
    }

    let mut file = tokio::fs::File::open(&canonical)
        .await
        .map_err(|_| path_not_allowed())?;
    // `File::metadata` on Unix is an `fstat` of the already-open handle, not
    // a fresh path lookup — the authoritative check is the `(dev, ino)`
    // comparison below; the stat above is only what makes it safe to reach
    // this `open` at all.
    let handle_meta = file.metadata().await.map_err(|_| path_not_allowed())?;
    if !handle_meta.is_file() {
        return Err(path_not_allowed());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let path_meta = std::fs::metadata(&canonical).map_err(|_| path_not_allowed())?;
        if path_meta.dev() != handle_meta.dev() || path_meta.ino() != handle_meta.ino() {
            return Err(path_not_allowed());
        }
    }

    if handle_meta.len() > UPLOAD_FILE_MAX_BYTES {
        return Err(upload_path_too_large(handle_meta.len()));
    }

    // Reads from the already-validated handle, not a fresh open of
    // `canonical`: a second open-by-path here would reintroduce the exact
    // TOCTOU window the fstat check above exists to close.
    let mut contents = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut file, &mut contents)
        .await
        .map_err(|_| local_storage_error("read the requested file_path"))?;
    let inferred_filename = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .map(ToString::to_string);
    Ok((Bytes::from(contents), inferred_filename))
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

// --- list_files ---

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListFilesParams {
    /// The project to list Files-module entries for: numeric id or slug
    /// identifier.
    pub(crate) project_id: ProjectRef,
}

/// One entry from `GET /projects/{id}/files.json`. `filename`/`content_type`
/// are structured metadata (same treatment as `GetRedmineAttachmentOutput`),
/// so they are **not** boundary-wrapped; `description` is Redmine-authored
/// free text and is.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct FileEntryOut {
    pub(crate) id: u64,
    pub(crate) filename: String,
    pub(crate) filesize: u64,
    pub(crate) content_type: Option<String>,
    pub(crate) description: Option<String>,
    /// Passed through verbatim, modulo the `REDMINE_PUBLIC_URL` rewrite — a
    /// mechanical download URL, not free text.
    pub(crate) content_url: String,
    pub(crate) digest: Option<String>,
    pub(crate) downloads: Option<u64>,
    pub(crate) author: Option<IdNameOut>,
    /// The project version this file is attached to, when its container is
    /// a `Version` rather than the project itself.
    pub(crate) version: Option<IdNameOut>,
    pub(crate) created_on: DateTime<Utc>,
}

fn file_entry_out(
    boundary: &Boundary,
    rewrite: &ContentUrlRewrite<'_>,
    a: &Attachment,
) -> FileEntryOut {
    FileEntryOut {
        id: a.id,
        filename: a.filename.clone(),
        filesize: a.filesize,
        content_type: a.content_type.clone(),
        description: a
            .description
            .as_deref()
            .map(|d| boundary.wrap("attachment.description", d)),
        content_url: rewrite.apply(&a.content_url),
        digest: a.digest.clone(),
        downloads: a.downloads,
        author: a
            .author
            .as_ref()
            .map(|u| id_name_out(boundary, "user.name", u)),
        version: a
            .version
            .as_ref()
            .map(|v| id_name_out(boundary, "version.name", v)),
        created_on: a.created_on,
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ListFilesOutput {
    pub(crate) files: Vec<FileEntryOut>,
}

// --- delete_file ---

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteFileParams {
    /// The id of the attachment to delete, from `list_files` or an issue's
    /// `attachments`.
    pub(crate) file_id: u64,
    /// `DELETE /attachments/{id}.json` deletes *any* attachment this
    /// credential can reach — issue and wiki attachments too, not just
    /// project Files — and Redmine's API does not report which container a
    /// given attachment belongs to, so this server cannot restrict the
    /// scope for you. Must be `true` for the delete to proceed.
    #[serde(default)]
    pub(crate) confirm_delete_any_attachment: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct DeleteFileOutput {
    pub(crate) success: bool,
    pub(crate) deleted_file_id: u64,
}

// --- upload_file ---

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UploadFileParams {
    /// The project to attach the uploaded file to.
    pub(crate) project_id: ProjectRef,
    /// Name the file should have in Redmine. Required when using
    /// `content_base64`; inferred from the path when using `file_path`.
    pub(crate) filename: Option<String>,
    /// Raw file bytes, base64-encoded. Exactly one of `content_base64`/
    /// `file_path` must be set.
    pub(crate) content_base64: Option<String>,
    /// Absolute path to a file already on this server: inside
    /// `ATTACHMENTS_DIR` or a directory listed in
    /// `REDMINE_MCP_UPLOAD_FILE_ROOTS`. Limited to 50 MiB.
    pub(crate) file_path: Option<String>,
    /// Not supported by this server. Present only so a caller who sends it
    /// gets a precise `UNSUPPORTED_SOURCE` refusal instead of a schema
    /// error; use `content_base64` or `file_path` instead.
    pub(crate) source_url: Option<String>,
    /// Human-readable description shown in the Files module.
    pub(crate) description: Option<String>,
    /// Attach to this version instead of the project directly.
    pub(crate) version_id: Option<u64>,
}

// --- cleanup_attachment_files ---

#[derive(Debug, Serialize, JsonSchema)]
#[allow(
    clippy::struct_field_names,
    reason = "field names match the reference contract's documented {cleaned_files, cleaned_bytes, cleaned_mb} shape verbatim"
)]
pub(crate) struct CleanupAttachmentFilesOutput {
    pub(crate) cleaned_files: u64,
    pub(crate) cleaned_bytes: u64,
    pub(crate) cleaned_mb: f64,
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

    /// `GET /projects/{id}/files.json` — the project **Files** module only,
    /// not issue attachments and not DMSF.
    #[tool(
        description = "List files in a project's Files module (GET /projects/{id}/files.json) — not issue attachments (use get_redmine_issue for those) and not the DMSF plugin. Returns metadata only; call get_redmine_attachment with the returned id to download the actual bytes. Use this when the user asks what files are attached to a project.",
        input_schema = crate::tools::schema::input::<ListFilesParams>(),
        output_schema = crate::tools::schema::output::<ListFilesOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn list_files(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ListFilesParams>,
    ) -> Result<CallToolResult, McpError> {
        let project_ident = resolve_project_ref(params.project_id)?;
        let scoped = self.scoped(&ctx)?;
        let files = match scoped.list_project_files(&project_ident).await {
            Ok(files) => files,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let rewrite = self.content_url_rewrite();
        let files = files
            .iter()
            .map(|a| file_entry_out(&boundary, &rewrite, a))
            .collect();
        Ok(output::ok(&ListFilesOutput { files }, self.output_caps()))
    }

    /// `DELETE /attachments/{id}.json`. Redmine's endpoint deletes any
    /// attachment regardless of container; since the API never reports
    /// `container_type` (see `redmine_client::model::attachment::Attachment`'s
    /// doc comment), the confirmation guard below is unconditional rather
    /// than scoped to non-project attachments.
    #[tool(
        description = "Delete a Redmine attachment by id. This can delete ANY attachment this credential can reach, not just project Files — issue and wiki attachments too — since Redmine does not report which container an attachment belongs to. Requires confirm_delete_any_attachment=true. Use this when the user explicitly asks to delete an attachment. Write tool; blocked in read-only mode.",
        input_schema = crate::tools::schema::input::<DeleteFileParams>(),
        output_schema = crate::tools::schema::output::<DeleteFileOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn delete_file(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<DeleteFileParams>,
    ) -> Result<CallToolResult, McpError> {
        if !params.confirm_delete_any_attachment {
            return Ok(err(
                ErrorCode::ConfirmationRequired,
                "deleting an attachment requires confirm_delete_any_attachment=true; Redmine's API does not indicate whether this id is a project file or an issue/wiki attachment, so this server cannot restrict the scope for you",
                Some(
                    "retry with confirm_delete_any_attachment=true if you intend to delete this attachment",
                ),
            ));
        }

        let scoped = self.scoped(&ctx)?;
        if let Err(e) = scoped.delete_attachment(AttachmentId(params.file_id)).await {
            return Ok(to_tool_error(e));
        }

        Ok(output::ok(
            &DeleteFileOutput {
                success: true,
                deleted_file_id: params.file_id,
            },
            self.output_caps(),
        ))
    }

    /// The two-step attach flow: `POST /uploads.json` for a token (raw
    /// bytes, `content_base64` decoded or `file_path` read locally per
    /// [`read_and_validate_upload_path`]), then `POST
    /// /projects/{id}/files.json` to attach it. Redmine answers the second
    /// call `204 No Content`, so the id from the first call is re-fetched
    /// via `GET /attachments/{id}.json` for the response.
    #[tool(
        description = "Upload a file and attach it to a project's Files module. Exactly one of content_base64 (requires filename) or file_path is required; source_url is not supported and returns UNSUPPORTED_SOURCE. file_path must be inside ATTACHMENTS_DIR or REDMINE_MCP_UPLOAD_FILE_ROOTS, capped at 50 MiB. Use this when attaching a file to a project. Write tool; blocked in read-only mode.",
        input_schema = crate::tools::schema::input::<UploadFileParams>(),
        output_schema = crate::tools::schema::output::<FileEntryOut>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn upload_file(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<UploadFileParams>,
    ) -> Result<CallToolResult, McpError> {
        let UploadFileParams {
            project_id,
            mut filename,
            content_base64,
            file_path,
            source_url,
            description,
            version_id,
        } = params;

        let sources_set = [
            content_base64.is_some(),
            file_path.is_some(),
            source_url.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if sources_set != 1 {
            return Ok(source_required("upload_file"));
        }
        if source_url.is_some() {
            return Ok(unsupported_source());
        }

        let body: Bytes = if let Some(b64) = content_base64 {
            if filename.is_none() {
                return Err(McpError::invalid_params(
                    "filename is required when using content_base64",
                    None,
                ));
            }
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(b64.as_bytes())
                .map_err(|e| {
                    McpError::invalid_params(
                        format!("content_base64 is not valid base64: {e}"),
                        None,
                    )
                })?;
            Bytes::from(decoded)
        } else {
            // `sources_set == 1` and `source_url`/`content_base64` are both
            // excluded above, so `file_path` must be set.
            let raw_path = file_path.unwrap_or_default();
            let store = self.attachments();
            let (contents, inferred) = match read_and_validate_upload_path(
                &self.inner.config.attachments.upload_file_roots,
                store.dir(),
                &raw_path,
            )
            .await
            {
                Ok(v) => v,
                Err(result) => return Ok(result),
            };
            if filename.is_none() {
                filename = inferred;
            }
            contents
        };

        let project_ident = resolve_project_ref(project_id)?;
        let scoped = self.scoped(&ctx)?;

        // A 413/422 from this specific step is a size condition, not a
        // validation one — Redmine's own `attachment_max_size` setting, not
        // `create_project_file`'s token/version_id validation (left to the
        // generic mapping below).
        let upload = match mint_upload_token(&scoped, body, filename.as_deref()).await {
            Ok(u) => u,
            Err(result) => return Ok(result),
        };

        let new_file = ProjectFileCreate {
            token: upload.token,
            filename: None,
            content_type: None,
            description,
            version_id,
        };
        if let Err(e) = scoped.create_project_file(&project_ident, &new_file).await {
            return Ok(to_tool_error(e));
        }

        let attachment = match scoped.get_attachment(AttachmentId(upload.id)).await {
            Ok(a) => a,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let rewrite = self.content_url_rewrite();
        Ok(output::ok(
            &file_entry_out(&boundary, &rewrite, &attachment),
            self.output_caps(),
        ))
    }

    /// Runs the same expiry sweep the background task performs
    /// ([`AttachmentStore::sweep_expired`]) on demand. Local-disk-only —
    /// never touches Redmine — so unlike every other write tool it is
    /// **not** gated by `REDMINE_MCP_READ_ONLY`; instead it is removed
    /// from the router entirely unless `REDMINE_MCP_EXPOSE_ADMIN_TOOLS=true`
    /// (see `RedmineMcp::new`).
    #[tool(
        description = "Immediately sweep expired files out of the local attachment store, the same cleanup the background sweeper performs on a timer, and report how much was reclaimed. Local-disk-only; never touches Redmine, so it still works in read-only mode. Use this to free disk space now instead of waiting for CLEANUP_INTERVAL_MINUTES. Admin tool, requires REDMINE_MCP_EXPOSE_ADMIN_TOOLS=true.",
        output_schema = crate::tools::schema::output::<CleanupAttachmentFilesOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
    )]
    pub(crate) async fn cleanup_attachment_files(&self) -> Result<CallToolResult, McpError> {
        let result = self.attachments().sweep_expired().await;
        #[allow(
            clippy::cast_precision_loss,
            reason = "an approximate MB figure for a human-readable summary; exact byte counts are in cleaned_bytes"
        )]
        let cleaned_mb = result.removed_bytes as f64 / (1024.0 * 1024.0);
        Ok(output::ok(
            &CleanupAttachmentFilesOutput {
                cleaned_files: result.removed_files,
                cleaned_bytes: result.removed_bytes,
                cleaned_mb,
            },
            self.output_caps(),
        ))
    }
}

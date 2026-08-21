//! `manage_document` (DMSF plugin, `redmine_dmsf`). Registered only when
//! `REDMINE_DMSF_ENABLED=true` — see `server.rs`'s `PLUGIN_TOOLS` gating
//! table.
//!
//! The plugin's wire shapes here are synthetic, derived from the reference
//! implementation's handling of the plugin rather than a live capture — see
//! `crates/redmine-client/tests/fixtures/README.md`'s plugin fixtures
//! section. Unlike the other three plugin families, `redmine_dmsf` is
//! open-source (GPL v2), so its wire shapes could in principle be verified
//! against a live instance. Parameters are flat and typed rather than the
//! reference's untyped `fields` dict (P4): an unknown key is rejected, not
//! silently dropped.

use std::str::FromStr as _;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use redmine_client::model::plugins::dmsf::{
    DmsfCommitRequest, DmsfRevisionWrite, DmsfUploadedFile, DmsfVersion,
};
use redmine_client::{DmsfFolderId, DocumentId};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::to_tool_error;
use crate::render::Boundary;
use crate::server::RedmineMcp;
use crate::tools::custom_fields::{CustomFieldEntry, custom_field_entries_to_write};
use crate::tools::discovery::{ProjectRef, resolve_project_ref};
use crate::tools::files;
use crate::tools::issues::IdNameOut;
use crate::tools::output::{self, ErrorCode, Pagination};

const LIST_MIN_LIMIT: u32 = 1;
const LIST_MAX_LIMIT: u32 = 100;
const LIST_DEFAULT_LIMIT: u32 = 100;

fn clamp_limit(limit: Option<u32>) -> u32 {
    limit
        .unwrap_or(LIST_DEFAULT_LIMIT)
        .clamp(LIST_MIN_LIMIT, LIST_MAX_LIMIT)
}

// --- shared output shape ---

/// One DMSF node. `filename`/`name`/`title`/`author.name` are structured
/// identifiers, not free text a project member wrote — **not**
/// boundary-wrapped (D12), unlike `description`/`comment`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct DocumentOut {
    pub(crate) id: u64,
    #[serde(rename = "type")]
    pub(crate) kind: Option<String>,
    pub(crate) filename: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) size: Option<u64>,
    pub(crate) content_type: Option<String>,
    pub(crate) folder_id: Option<u64>,
    pub(crate) project_id: Option<u64>,
    pub(crate) author: Option<IdNameOut>,
    pub(crate) created_on: Option<DateTime<Utc>>,
    pub(crate) updated_on: Option<DateTime<Utc>>,
}

fn document_out(
    boundary: &Boundary,
    n: &redmine_client::model::plugins::dmsf::DmsfNode,
) -> DocumentOut {
    DocumentOut {
        id: n.id,
        kind: n.kind.clone(),
        filename: n.filename.clone(),
        title: n.title.clone(),
        name: n.name.clone(),
        description: n
            .description
            .as_deref()
            .map(|d| boundary.wrap("document.description", d)),
        version: n.version.clone(),
        size: n.size,
        content_type: n.content_type.clone(),
        folder_id: n.folder_id,
        project_id: n.project_id,
        author: n.author.as_ref().map(|a| IdNameOut {
            id: a.id,
            name: a.name.clone(),
        }),
        created_on: n.created_on,
        updated_on: n.updated_on,
    }
}

fn document_not_found() -> CallToolResult {
    output::err(
        ErrorCode::NotFound,
        "the requested DMSF document was not found or carries no revision this client can \
         make sense of",
        Some(
            "verify document_id is correct; it may have been deleted, or the credential cannot see it",
        ),
    )
}

// --- manage_document ---

/// DMSF exposes no delete action from this tool — matching the reference.
/// Deletion semantics (revision vs. file vs. folder, recycle bin) are a
/// design question, not an omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ManageDocumentAction {
    List,
    Get,
    Create,
    Update,
}

impl ManageDocumentAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Get => "get",
            Self::Create => "create",
            Self::Update => "update",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManageDocumentParams {
    /// Operation to perform. There is no `delete` action.
    pub(crate) action: ManageDocumentAction,
    /// The project to act on: numeric id or slug identifier. Required for
    /// `list` and `create`.
    #[serde(default)]
    pub(crate) project_id: Option<ProjectRef>,
    /// For `list`, restrict to one folder (omit for the whole project). For
    /// `create`, the destination folder (omit for the project root).
    #[serde(default)]
    pub(crate) folder_id: Option<u64>,
    /// For `list`, max results per call, clamped to 1-100. Default 100.
    #[serde(default)]
    pub(crate) limit: Option<u32>,
    /// For `list`, pagination offset. Default 0.
    #[serde(default)]
    pub(crate) offset: Option<u64>,
    /// The document to act on. Required for `get` and `update`.
    #[serde(default)]
    pub(crate) document_id: Option<u64>,
    /// Raw file bytes, base64-encoded. For `create`; exactly one of
    /// `content_base64`/`file_path` is required. Limited to 50 MiB decoded.
    #[serde(default)]
    pub(crate) content_base64: Option<String>,
    /// Absolute path to a file already on this server: inside
    /// `ATTACHMENTS_DIR` or a directory listed in
    /// `REDMINE_MCP_UPLOAD_FILE_ROOTS`. For `create`.
    #[serde(default)]
    pub(crate) file_path: Option<String>,
    /// Not supported by this server. Present only so a caller who sends it
    /// gets a precise `UNSUPPORTED_SOURCE` refusal instead of a schema
    /// error; use `content_base64` or `file_path` instead.
    #[serde(default)]
    pub(crate) source_url: Option<String>,
    /// The stored filename (DMSF's own `name` field, trap 2). For `create`,
    /// required when using `content_base64`; inferred from `file_path`
    /// otherwise. For `update`, a new filename — defaults to the document's
    /// current one if omitted.
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// Display title. For `create`/`update`; on `update`, defaults to the
    /// document's current title if omitted (trap 1: the plugin 500s on a
    /// missing title, so this server never sends one blank).
    #[serde(default)]
    pub(crate) title: Option<String>,
    /// Free-text description. For `create`/`update`.
    #[serde(default)]
    pub(crate) description: Option<String>,
    /// A revision comment. For `create`/`update`.
    #[serde(default)]
    pub(crate) comment: Option<String>,
    /// `"X"`, `"X.Y"`, or `"X.Y.Z"`, each part a non-negative integer. For
    /// `create` only — `update`'s endpoint reads version fields from a
    /// different place and DMSF auto-increments the patch version on every
    /// revision regardless of what is asked.
    #[serde(default)]
    pub(crate) version: Option<String>,
    /// Custom field values to set, by id. For `create`/`update`.
    #[serde(default)]
    pub(crate) custom_fields: Option<Vec<CustomFieldEntry>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ManageDocumentOutput {
    pub(crate) success: bool,
    /// Populated for `action = "list"`.
    pub(crate) documents: Option<Vec<DocumentOut>>,
    pub(crate) pagination: Option<Pagination>,
    /// Populated for `action = "get"` only — `create` returns
    /// `document_id` instead, since the commit response is too sparse to
    /// build a full [`DocumentOut`] from (see `note`).
    pub(crate) document: Option<DocumentOut>,
    /// Populated for `action = "create"`/`"update"`.
    pub(crate) document_id: Option<u64>,
    /// Names of the fields this call changed. `action = "update"` only.
    pub(crate) updated_fields: Option<Vec<&'static str>>,
    /// Clarifying note. `create`: the response is sparse; call `get` for
    /// full metadata. `update`: a new revision was created; earlier ones
    /// survive.
    pub(crate) note: Option<String>,
}

fn read_only_refusal(action: ManageDocumentAction) -> CallToolResult {
    output::err(
        ErrorCode::ReadOnly,
        format!(
            "this server is running in read-only mode; manage_document(action=\"{}\") is disabled",
            action.as_str()
        ),
        Some(
            "use action=\"list\" or action=\"get\" instead, or ask the operator to disable read-only mode",
        ),
    )
}

fn require_document_id(
    params: &ManageDocumentParams,
    action: ManageDocumentAction,
) -> Result<u64, McpError> {
    params.document_id.ok_or_else(|| {
        McpError::invalid_params(
            format!("document_id is required for action=\"{}\"", action.as_str()),
            None,
        )
    })
}

fn require_project_ident(
    params: &ManageDocumentParams,
    action: ManageDocumentAction,
) -> Result<redmine_client::ProjectIdent, McpError> {
    let r = params.project_id.clone().ok_or_else(|| {
        McpError::invalid_params(
            format!("project_id is required for action=\"{}\"", action.as_str()),
            None,
        )
    })?;
    resolve_project_ref(r)
}

#[tool_router(router = dmsf_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// `list`: `GET /projects/{pid}/dmsf.json`. `get`:
    /// `GET /dmsf_files/{id}.json`. `create`: `POST /uploads.json` then
    /// `POST /projects/{pid}/dmsf/commit.json`. `update`:
    /// `POST /dmsf/files/{id}/revision/create.json` — every update creates a
    /// new revision; earlier ones survive.
    #[tool(
        description = "List, get, create, or update documents in the DMSF plugin (redmine_dmsf, GPL v2; must be installed server-side, and its DMSF module replaces rather than complements Redmine's built-in Documents). There is no delete action. list/get work in read-only mode; create/update are blocked. create requires project_id and exactly one of content_base64 (requires name) or file_path, both capped at 50 MiB; its response is sparse ({document_id} only) — follow up with action=\"get\". update always creates a new revision rather than replacing one, and requires document_id.",
        input_schema = crate::tools::schema::input::<ManageDocumentParams>(),
        output_schema = crate::tools::schema::output::<ManageDocumentOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn manage_document(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ManageDocumentParams>,
    ) -> Result<CallToolResult, McpError> {
        let boundary = Boundary::new();

        match params.action {
            ManageDocumentAction::List => {
                let project_ident = require_project_ident(&params, ManageDocumentAction::List)?;
                let scoped = self.scoped(&ctx)?;
                let limit = clamp_limit(params.limit);
                let offset = params.offset.unwrap_or(0);
                let folder_id = params.folder_id.map(DmsfFolderId);
                let page = match scoped
                    .list_dmsf_nodes(&project_ident, folder_id, limit, offset)
                    .await
                {
                    Ok(page) => page,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                let pagination = Pagination::from_page(&page);
                let documents = page
                    .items
                    .iter()
                    .map(|n| document_out(&boundary, n))
                    .collect();
                Ok(output::ok(
                    &ManageDocumentOutput {
                        success: true,
                        documents: Some(documents),
                        pagination: Some(pagination),
                        document: None,
                        document_id: None,
                        updated_fields: None,
                        note: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageDocumentAction::Get => {
                let document_id = require_document_id(&params, ManageDocumentAction::Get)?;
                let scoped = self.scoped(&ctx)?;
                let node = match scoped.get_dmsf_file(DocumentId(document_id)).await {
                    Ok(Some(node)) => node,
                    Ok(None) => return Ok(document_not_found()),
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageDocumentOutput {
                        success: true,
                        documents: None,
                        pagination: None,
                        document: Some(document_out(&boundary, &node)),
                        document_id: None,
                        updated_fields: None,
                        note: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageDocumentAction::Create => {
                if self.inner.config.read_only {
                    return Ok(read_only_refusal(params.action));
                }

                // D7: validate the version string first — zero requests for
                // a malformed one, so no upload token is ever orphaned.
                let version = match &params.version {
                    Some(v) => match DmsfVersion::from_str(v) {
                        Ok(v) => Some(v),
                        Err(e) => return Err(McpError::invalid_params(e, None)),
                    },
                    None => None,
                };

                let sources_set = [
                    params.content_base64.is_some(),
                    params.file_path.is_some(),
                    params.source_url.is_some(),
                ]
                .into_iter()
                .filter(|present| *present)
                .count();
                if sources_set != 1 {
                    return Ok(files::source_required("manage_document"));
                }
                if params.source_url.is_some() {
                    return Ok(files::unsupported_source());
                }

                let mut name = params.name.clone();
                let body: Bytes = if let Some(b64) = params.content_base64.clone() {
                    if name.is_none() {
                        return Err(McpError::invalid_params(
                            "name is required for action=\"create\" when using content_base64",
                            None,
                        ));
                    }
                    match files::decode_upload_base64(
                        "manage_document",
                        &b64,
                        files::UPLOAD_FILE_MAX_BYTES,
                    ) {
                        Ok(bytes) => bytes,
                        Err(files::Base64UploadError::Malformed(e)) => {
                            return Err(McpError::invalid_params(
                                format!("content_base64 is not valid base64: {e}"),
                                None,
                            ));
                        }
                        Err(files::Base64UploadError::TooLarge(result)) => return Ok(result),
                    }
                } else {
                    // `sources_set == 1` and `source_url`/`content_base64`
                    // are both excluded above, so `file_path` must be set.
                    let raw_path = params.file_path.clone().unwrap_or_default();
                    let store = self.attachments();
                    let (contents, inferred) = match files::read_and_validate_upload_path(
                        &self.inner.config.attachments.upload_file_roots,
                        store.dir(),
                        "manage_document",
                        files::UPLOAD_FILE_MAX_BYTES,
                        &raw_path,
                    )
                    .await
                    {
                        Ok(v) => v,
                        Err(result) => return Ok(result),
                    };
                    if name.is_none() {
                        name = inferred;
                    }
                    contents
                };
                let Some(name) = name else {
                    return Err(McpError::invalid_params(
                        "name could not be determined from file_path; provide it explicitly",
                        None,
                    ));
                };

                let project_ident = require_project_ident(&params, ManageDocumentAction::Create)?;
                let scoped = self.scoped(&ctx)?;

                let upload = match files::mint_upload_token(&scoped, body, Some(&name)).await {
                    Ok(u) => u,
                    Err(result) => return Ok(result),
                };

                let uploaded_file = DmsfUploadedFile {
                    token: upload.token,
                    name,
                    title: params.title.clone(),
                    description: params.description.clone(),
                    comment: params.comment.clone(),
                    version_major: version.map(|v| v.major),
                    version_minor: version.map(|v| v.minor),
                    version_patch: version.map(|v| v.patch),
                    custom_field_values: params
                        .custom_fields
                        .clone()
                        .map(custom_field_entries_to_write),
                };
                let req = DmsfCommitRequest {
                    uploaded_file,
                    folder_id: params.folder_id,
                };
                // The upload above already succeeded: a failure from here
                // on leaves an orphaned token server-side (Risk 1). The
                // message says so explicitly rather than implying the whole
                // call is retry-safe.
                let nodes = match scoped.commit_dmsf_upload(&project_ident, &req).await {
                    Ok(nodes) => nodes,
                    Err(e) => {
                        let mut result = to_tool_error(e);
                        if let Some(payload) = result.structured_content.as_mut()
                            && let Some(message) = payload.get("error").and_then(|v| v.as_str())
                        {
                            let message = format!(
                                "the file was uploaded successfully, but committing it to DMSF failed: {message}"
                            );
                            payload["error"] = serde_json::Value::String(message);
                        }
                        return Ok(result);
                    }
                };
                let document_id = nodes.first().map(|n| n.id);
                let note = document_id.map_or_else(
                    || {
                        "the commit response named no document id; call action=\"list\" to find \
                         the new document"
                            .to_string()
                    },
                    |id| {
                        format!(
                            "the commit response is sparse by design (id/name only); call \
                             manage_document(action=\"get\", document_id={id}) for full metadata"
                        )
                    },
                );
                Ok(output::ok(
                    &ManageDocumentOutput {
                        success: true,
                        documents: None,
                        pagination: None,
                        document: None,
                        document_id,
                        updated_fields: None,
                        note: Some(note),
                    },
                    self.output_caps(),
                ))
            }
            ManageDocumentAction::Update => {
                if self.inner.config.read_only {
                    return Ok(read_only_refusal(params.action));
                }
                let document_id = require_document_id(&params, ManageDocumentAction::Update)?;
                if params.title.is_none()
                    && params.name.is_none()
                    && params.description.is_none()
                    && params.comment.is_none()
                    && params.custom_fields.is_none()
                {
                    return Err(McpError::invalid_params(
                        "at least one field to update is required",
                        None,
                    ));
                }

                let scoped = self.scoped(&ctx)?;
                // D4: always pre-fetch, so trap 1 (a missing title/name
                // 500s the server) can never be triggered by an
                // under-specified caller. No write happens if this fails.
                let current = match scoped.get_dmsf_file(DocumentId(document_id)).await {
                    Ok(Some(node)) => node,
                    Ok(None) => return Ok(document_not_found()),
                    Err(e) => return Ok(to_tool_error(e)),
                };

                let mut updated_fields: Vec<&'static str> = Vec::new();
                if params.title.is_some() {
                    updated_fields.push("title");
                }
                if params.name.is_some() {
                    updated_fields.push("name");
                }
                if params.description.is_some() {
                    updated_fields.push("description");
                }
                if params.comment.is_some() {
                    updated_fields.push("comment");
                }
                if params.custom_fields.is_some() {
                    updated_fields.push("custom_fields");
                }

                let title = params.title.clone().or(current.title).unwrap_or_default();
                let name = params.name.clone().or(current.name).unwrap_or_default();
                let revision = DmsfRevisionWrite {
                    title,
                    name,
                    description: params.description.clone(),
                    comment: params.comment.clone(),
                    custom_field_values: params
                        .custom_fields
                        .clone()
                        .map(custom_field_entries_to_write),
                };
                if let Err(e) = scoped
                    .create_dmsf_revision(DocumentId(document_id), &revision)
                    .await
                {
                    return Ok(to_tool_error(e));
                }

                Ok(output::ok(
                    &ManageDocumentOutput {
                        success: true,
                        documents: None,
                        pagination: None,
                        document: None,
                        document_id: Some(document_id),
                        updated_fields: Some(updated_fields),
                        note: Some(
                            "a new revision was created; earlier revisions still exist".to_string(),
                        ),
                    },
                    self.output_caps(),
                ))
            }
        }
    }
}

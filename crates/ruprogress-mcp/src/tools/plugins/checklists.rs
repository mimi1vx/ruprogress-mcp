//! `get_checklist`, `create_checklist_item`, `update_checklist_item`
//! (`RedmineUP` Checklists Pro plugin). Registered only when
//! `REDMINE_CHECKLISTS_ENABLED=true` — see `server.rs`'s `PLUGIN_TOOLS`
//! gating table.
//!
//! The plugin's wire shapes here are synthetic, derived from the reference
//! implementation's handling of the plugin rather than a live capture
//! (Checklists Pro is commercial) — see
//! `crates/redmine-client/tests/fixtures/README.md`'s plugin fixtures
//! section.

use chrono::{DateTime, Utc};
use redmine_client::model::plugins::checklists::{
    ChecklistItem, ChecklistItemCreate, ChecklistItemUpdate,
};
use redmine_client::{ChecklistItemId, IssueId};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::to_tool_error;
use crate::render::Boundary;
use crate::server::RedmineMcp;
use crate::tools::output;

// --- shared output shape ---

/// A checklist item as returned to the caller. Renames the plugin's
/// `created_at`/`updated_at` wire fields to `created_on`/`updated_on`,
/// matching every other timestamp this server emits — a caller should not
/// have to learn that one plugin family spells it differently.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ChecklistItemOut {
    pub(crate) id: u64,
    pub(crate) subject: String,
    pub(crate) is_done: Option<bool>,
    pub(crate) is_section: Option<bool>,
    pub(crate) position: Option<u32>,
    pub(crate) created_on: Option<DateTime<Utc>>,
    pub(crate) updated_on: Option<DateTime<Utc>>,
}

fn checklist_item_out(boundary: &Boundary, item: &ChecklistItem) -> ChecklistItemOut {
    ChecklistItemOut {
        id: item.id,
        subject: boundary.wrap("checklist_item.subject", &item.subject),
        is_done: item.is_done,
        is_section: item.is_section,
        position: item.position,
        created_on: item.created_at,
        updated_on: item.updated_at,
    }
}

// --- get_checklist ---

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetChecklistParams {
    /// The issue whose checklist to retrieve.
    pub(crate) issue_id: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct GetChecklistOutput {
    pub(crate) issue_id: u64,
    pub(crate) total_count: u64,
    pub(crate) items: Vec<ChecklistItemOut>,
}

// --- create_checklist_item ---

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateChecklistItemParams {
    /// The issue to add the checklist item to.
    pub(crate) issue_id: u64,
    /// Text of the new checklist item, or the section header's title. Must
    /// not be blank.
    pub(crate) subject: String,
    /// `true` to create a section header rather than a checkable item.
    /// Default `false`.
    #[serde(default)]
    pub(crate) is_section: Option<bool>,
    /// Initial checked state for a checkable item. Sent as given even when
    /// `is_section=true` — the plugin, not this server, decides to ignore it
    /// there. Default `false`.
    #[serde(default)]
    pub(crate) is_done: Option<bool>,
    /// 1-based position in the checklist. Omit to append at the end.
    #[serde(default)]
    pub(crate) position: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct CreateChecklistItemOutput {
    pub(crate) success: bool,
    pub(crate) issue_id: u64,
    /// `None` when the plugin's response body carried no id — a real
    /// possibility with this endpoint, not a failure; call `get_checklist`
    /// to find it.
    pub(crate) checklist_item_id: Option<u64>,
    pub(crate) subject: String,
    pub(crate) is_section: bool,
    pub(crate) is_done: bool,
    pub(crate) position: Option<u32>,
}

// --- update_checklist_item ---

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateChecklistItemParams {
    /// The checklist item to update, from `get_checklist`.
    pub(crate) checklist_item_id: u64,
    /// New text, if changing it. Must not be blank if given.
    #[serde(default)]
    pub(crate) subject: Option<String>,
    /// New checked state, if changing it.
    #[serde(default)]
    pub(crate) is_done: Option<bool>,
    /// New 1-based position, if changing it.
    #[serde(default)]
    pub(crate) position: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct UpdateChecklistItemOutput {
    pub(crate) success: bool,
    pub(crate) checklist_item_id: u64,
    /// Names of the fields this call changed, e.g. `["subject", "is_done"]`.
    /// The plugin's `PUT` response body is undocumented and discarded — this
    /// echoes what was sent, not a re-fetched item.
    pub(crate) updated_fields: Vec<&'static str>,
}

#[tool_router(router = checklists_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// `GET /issues/{issue_id}/checklists.json`.
    #[tool(
        description = "Get the checklist items on an issue (RedmineUP Checklists Pro plugin). Use this to see an issue's checklist before adding or editing an item; an empty list means the issue has no checklist — do not retry. A very large checklist may be silently truncated by this server's response-size caps, with no further page to fetch.",
        input_schema = crate::tools::schema::input::<GetChecklistParams>(),
        output_schema = crate::tools::schema::output::<GetChecklistOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn get_checklist(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<GetChecklistParams>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let items = match scoped.list_checklist_items(IssueId(params.issue_id)).await {
            Ok(items) => items,
            Err(e) => return Ok(to_tool_error(e)),
        };
        let boundary = Boundary::new();
        let items: Vec<ChecklistItemOut> = items
            .iter()
            .map(|item| checklist_item_out(&boundary, item))
            .collect();
        let total_count = u64::try_from(items.len()).unwrap_or(u64::MAX);
        Ok(output::ok(
            &GetChecklistOutput {
                issue_id: params.issue_id,
                total_count,
                items,
            },
            self.output_caps(),
        ))
    }

    /// `POST /issues/{issue_id}/checklists.json`.
    #[tool(
        description = "Add a checklist item or section header to an issue (RedmineUP Checklists Pro plugin). Use this after get_checklist to add a new checkable item (is_section=false, the default) or a section header (is_section=true). Write tool; blocked in read-only mode.",
        input_schema = crate::tools::schema::input::<CreateChecklistItemParams>(),
        output_schema = crate::tools::schema::output::<CreateChecklistItemOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn create_checklist_item(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<CreateChecklistItemParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.subject.trim().is_empty() {
            return Err(McpError::invalid_params("subject must not be blank", None));
        }
        if let Some(position) = params.position
            && position < 1
        {
            return Err(McpError::invalid_params(
                "position must be a 1-based positive integer (>= 1)",
                None,
            ));
        }

        let scoped = self.scoped(&ctx)?;
        let is_section = params.is_section.unwrap_or(false);
        let is_done = params.is_done.unwrap_or(false);
        let new = ChecklistItemCreate {
            subject: params.subject.clone(),
            is_section: Some(is_section),
            is_done: Some(is_done),
            position: params.position,
        };
        let checklist_item_id = match scoped
            .create_checklist_item(IssueId(params.issue_id), &new)
            .await
        {
            Ok(id) => id,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        Ok(output::ok(
            &CreateChecklistItemOutput {
                success: true,
                issue_id: params.issue_id,
                checklist_item_id: checklist_item_id.map(|id| id.0),
                subject: boundary.wrap("checklist_item.subject", &params.subject),
                is_section,
                is_done,
                position: params.position,
            },
            self.output_caps(),
        ))
    }

    /// `PUT /checklists/{checklist_item_id}.json`.
    #[tool(
        description = "Edit a checklist item's text, done state, or position (RedmineUP Checklists Pro plugin). Use this after get_checklist to change one existing item; at least one of subject/is_done/position is required. Write tool; blocked in read-only mode.",
        input_schema = crate::tools::schema::input::<UpdateChecklistItemParams>(),
        output_schema = crate::tools::schema::output::<UpdateChecklistItemOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn update_checklist_item(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<UpdateChecklistItemParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(subject) = &params.subject
            && subject.trim().is_empty()
        {
            return Err(McpError::invalid_params(
                "subject must not be blank if given",
                None,
            ));
        }
        if let Some(position) = params.position
            && position < 1
        {
            return Err(McpError::invalid_params(
                "position must be a 1-based positive integer (>= 1)",
                None,
            ));
        }

        let mut updated_fields: Vec<&'static str> = Vec::new();
        if params.subject.is_some() {
            updated_fields.push("subject");
        }
        if params.is_done.is_some() {
            updated_fields.push("is_done");
        }
        if params.position.is_some() {
            updated_fields.push("position");
        }
        if updated_fields.is_empty() {
            return Err(McpError::invalid_params(
                "at least one of subject, is_done, position is required",
                None,
            ));
        }

        let scoped = self.scoped(&ctx)?;
        let patch = ChecklistItemUpdate {
            subject: params.subject,
            is_done: params.is_done,
            position: params.position,
        };
        if let Err(e) = scoped
            .update_checklist_item(ChecklistItemId(params.checklist_item_id), &patch)
            .await
        {
            return Ok(to_tool_error(e));
        }

        Ok(output::ok(
            &UpdateChecklistItemOutput {
                success: true,
                checklist_item_id: params.checklist_item_id,
                updated_fields,
            },
            self.output_caps(),
        ))
    }
}

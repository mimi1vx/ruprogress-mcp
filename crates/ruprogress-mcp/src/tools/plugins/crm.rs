//! `manage_contact` (`RedmineUP` CRM plugin). Registered only when
//! `REDMINE_CRM_ENABLED=true` — see `server.rs`'s `PLUGIN_TOOLS` gating
//! table.
//!
//! The plugin's wire shapes here are synthetic, derived from the reference
//! implementation's handling of the plugin rather than a live capture (CRM
//! is commercial) — see
//! `crates/redmine-client/tests/fixtures/README.md`'s plugin fixtures
//! section. Parameters are flat and typed rather than the reference's
//! untyped `fields` dict (P4/R2): an unknown key is rejected, not silently
//! dropped.
//!
//! Contact PII (`email`, `phone`, `address`, `birthday`, `website`) is
//! never logged and never named in an error message (errors reference
//! `contact_id` only) — enforced by
//! `tests/tools_plugins_products_crm.rs`'s log-capture test (P9/R9).

use chrono::{DateTime, Utc};
use redmine_client::model::plugins::crm::{
    Contact, ContactAddressWrite, ContactInclude, ContactQuery, ContactWrite,
};
use redmine_client::{ContactId, ProjectIdent};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::to_tool_error;
use crate::render::Boundary;
use crate::server::RedmineMcp;
use crate::tools::discovery::{ProjectRef, resolve_project_ref};
use crate::tools::issues::{IdNameOut, id_name_out};
use crate::tools::output::{self, ErrorCode, Pagination};

const LIST_MIN_LIMIT: u32 = 1;
const LIST_MAX_LIMIT: u32 = 100;
const LIST_DEFAULT_LIMIT: u32 = 100;

fn clamp_limit(limit: Option<u32>) -> u32 {
    limit
        .unwrap_or(LIST_DEFAULT_LIMIT)
        .clamp(LIST_MIN_LIMIT, LIST_MAX_LIMIT)
}

fn validate_visibility(visibility: Option<u8>) -> Result<(), McpError> {
    match visibility {
        None | Some(0..=2) => Ok(()),
        Some(other) => Err(McpError::invalid_params(
            format!("visibility must be 0 (Project), 1 (Public), or 2 (Private), got {other}"),
            None,
        )),
    }
}

// --- include (R11): a typed enum vector, not a free string ---

/// `get`'s `include` values, matching how `IssueInclude` is handled at the
/// tool boundary (`model/issue.rs`): unknown values are an argument error,
/// not a 500 from the plugin.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContactIncludeParam {
    Notes,
    Deals,
    Contacts,
}

impl From<ContactIncludeParam> for ContactInclude {
    fn from(p: ContactIncludeParam) -> Self {
        match p {
            ContactIncludeParam::Notes => Self::Notes,
            ContactIncludeParam::Deals => Self::Deals,
            ContactIncludeParam::Contacts => Self::Contacts,
        }
    }
}

// --- address (R7): a typed nested struct, not an untyped dict ---

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContactAddressParams {
    #[serde(default)]
    pub(crate) street1: Option<String>,
    #[serde(default)]
    pub(crate) street2: Option<String>,
    #[serde(default)]
    pub(crate) city: Option<String>,
    #[serde(default)]
    pub(crate) region: Option<String>,
    #[serde(default)]
    pub(crate) country: Option<String>,
    #[serde(default)]
    pub(crate) postcode: Option<String>,
}

impl From<ContactAddressParams> for ContactAddressWrite {
    fn from(p: ContactAddressParams) -> Self {
        Self {
            street1: p.street1,
            street2: p.street2,
            city: p.city,
            region: p.region,
            country: p.country,
            postcode: p.postcode,
        }
    }
}

// --- shared output shape ---
//
// R9: `first_name`/`last_name`/`middle_name`/`company`/`job_title`/
// `background`/`assigned_to.name`/`tags` are boundary-wrapped (display
// fields a project member could have written). `phone`/`email`/`website`/
// `skype_name`/`birthday`/the address sub-fields are PII the caller asked
// for and are returned unwrapped, per R9 — `background` is the one
// exception among the free-text-shaped fields, wrapped because it is
// long-form free text rather than a short PII value.

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ContactAddressOut {
    pub(crate) street1: Option<String>,
    pub(crate) street2: Option<String>,
    pub(crate) city: Option<String>,
    pub(crate) region: Option<String>,
    pub(crate) country: Option<String>,
    pub(crate) postcode: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ContactOut {
    pub(crate) id: u64,
    pub(crate) first_name: Option<String>,
    pub(crate) last_name: Option<String>,
    pub(crate) middle_name: Option<String>,
    pub(crate) company: Option<String>,
    pub(crate) job_title: Option<String>,
    pub(crate) phone: Option<String>,
    pub(crate) email: Option<String>,
    pub(crate) website: Option<String>,
    pub(crate) skype_name: Option<String>,
    pub(crate) birthday: Option<String>,
    pub(crate) background: Option<String>,
    pub(crate) address: Option<ContactAddressOut>,
    pub(crate) is_company: Option<bool>,
    pub(crate) tags: Option<Vec<String>>,
    pub(crate) visibility: Option<u8>,
    pub(crate) assigned_to: Option<IdNameOut>,
    pub(crate) created_on: Option<DateTime<Utc>>,
    pub(crate) updated_on: Option<DateTime<Utc>>,
}

fn contact_out(boundary: &Boundary, c: &Contact) -> ContactOut {
    ContactOut {
        id: c.id,
        first_name: c
            .first_name
            .as_deref()
            .map(|s| boundary.wrap("contact.first_name", s)),
        last_name: c
            .last_name
            .as_deref()
            .map(|s| boundary.wrap("contact.last_name", s)),
        middle_name: c
            .middle_name
            .as_deref()
            .map(|s| boundary.wrap("contact.middle_name", s)),
        company: c
            .company
            .as_deref()
            .map(|s| boundary.wrap("contact.company", s)),
        job_title: c
            .job_title
            .as_deref()
            .map(|s| boundary.wrap("contact.job_title", s)),
        phone: c.phone.clone(),
        email: c.email.clone(),
        website: c.website.clone(),
        skype_name: c.skype_name.clone(),
        birthday: c.birthday.clone(),
        background: c
            .background
            .as_deref()
            .map(|s| boundary.wrap("contact.background", s)),
        address: c.address.as_ref().map(|a| ContactAddressOut {
            street1: a.street1.clone(),
            street2: a.street2.clone(),
            city: a.city.clone(),
            region: a.region.clone(),
            country: a.country.clone(),
            postcode: a.postcode.clone(),
        }),
        is_company: c.is_company,
        tags: c.tags.as_ref().map(|tags| {
            tags.iter()
                .map(|t| boundary.wrap("contact.tag", t))
                .collect()
        }),
        visibility: c.visibility,
        assigned_to: c
            .assigned_to
            .as_ref()
            .map(|u| id_name_out(boundary, "contact.assigned_to.name", u)),
        created_on: c.created_on,
        updated_on: c.updated_on,
    }
}

// --- manage_contact ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManageContactAction {
    List,
    Get,
    Create,
    Update,
    Delete,
    AssignToProject,
    RemoveFromProject,
}

impl ManageContactAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Get => "get",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::AssignToProject => "assign_to_project",
            Self::RemoveFromProject => "remove_from_project",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManageContactParams {
    /// Operation to perform.
    pub(crate) action: ManageContactAction,
    /// For `list`, optional project filter. For `create`, required (the
    /// project to associate the new contact with). For
    /// `assign_to_project`/`remove_from_project`, the project to attach to
    /// or detach from.
    #[serde(default)]
    pub(crate) project_id: Option<ProjectRef>,
    /// For `list`, free-text search (matches name/company/email).
    #[serde(default)]
    pub(crate) search: Option<String>,
    /// For `list`, a comma-separated tag filter, passed through as given
    /// (this is a filter expression the plugin parses, not a set of values
    /// being written).
    #[serde(default)]
    pub(crate) tags: Option<String>,
    /// For `list`, filter by assignee user id. For `create`/`update`, the
    /// user to assign the contact to.
    #[serde(default)]
    pub(crate) assigned_to_id: Option<u64>,
    /// For `list`, max results per call, clamped to 1-100. Default 100.
    #[serde(default)]
    pub(crate) limit: Option<u32>,
    /// For `list`, pagination offset. Default 0.
    #[serde(default)]
    pub(crate) offset: Option<u64>,
    /// The contact to act on. Required for every action except `list` and
    /// `create`.
    #[serde(default)]
    pub(crate) contact_id: Option<u64>,
    /// For `get`, additional data to include.
    #[serde(default)]
    pub(crate) include: Option<Vec<ContactIncludeParam>>,
    /// Given name. Required for `create`.
    #[serde(default)]
    pub(crate) first_name: Option<String>,
    #[serde(default)]
    pub(crate) last_name: Option<String>,
    #[serde(default)]
    pub(crate) middle_name: Option<String>,
    #[serde(default)]
    pub(crate) company: Option<String>,
    #[serde(default)]
    pub(crate) job_title: Option<String>,
    #[serde(default)]
    pub(crate) phone: Option<String>,
    #[serde(default)]
    pub(crate) email: Option<String>,
    #[serde(default)]
    pub(crate) website: Option<String>,
    #[serde(default)]
    pub(crate) skype_name: Option<String>,
    /// `YYYY-MM-DD`.
    #[serde(default)]
    pub(crate) birthday: Option<String>,
    #[serde(default)]
    pub(crate) background: Option<String>,
    #[serde(default)]
    pub(crate) address: Option<ContactAddressParams>,
    /// `true` to mark this contact as a company rather than a person.
    /// Default `false`.
    #[serde(default)]
    pub(crate) is_company: Option<bool>,
    /// `0` = Project (default), `1` = Public, `2` = Private.
    #[serde(default)]
    pub(crate) visibility: Option<u8>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ManageContactOutput {
    pub(crate) success: bool,
    /// Populated for `action = "list"`.
    pub(crate) contacts: Option<Vec<ContactOut>>,
    pub(crate) pagination: Option<Pagination>,
    /// Populated for `action = "get"`/`"create"`/`"update"`.
    pub(crate) contact: Option<ContactOut>,
    /// Names of the fields this call changed. `action = "update"` only.
    pub(crate) updated_fields: Option<Vec<&'static str>>,
    /// Populated for `action = "delete"`.
    pub(crate) deleted_contact_id: Option<u64>,
    /// A human-readable note. Set on `assign_to_project`/
    /// `remove_from_project` to make explicit that neither action creates
    /// or deletes the contact itself.
    pub(crate) message: Option<String>,
}

fn read_only_refusal(action: ManageContactAction) -> CallToolResult {
    output::err(
        ErrorCode::ReadOnly,
        format!(
            "this server is running in read-only mode; manage_contact(action=\"{}\") is disabled",
            action.as_str()
        ),
        Some(
            "use action=\"list\" or action=\"get\" instead, or ask the operator to disable read-only mode",
        ),
    )
}

fn require_contact_id(
    params: &ManageContactParams,
    action: ManageContactAction,
) -> Result<u64, McpError> {
    params.contact_id.ok_or_else(|| {
        McpError::invalid_params(
            format!("contact_id is required for action=\"{}\"", action.as_str()),
            None,
        )
    })
}

fn require_project_ident(
    params: &ManageContactParams,
    action: ManageContactAction,
) -> Result<ProjectIdent, McpError> {
    let project_ref = params.project_id.clone().ok_or_else(|| {
        McpError::invalid_params(
            format!("project_id is required for action=\"{}\"", action.as_str()),
            None,
        )
    })?;
    resolve_project_ref(project_ref)
}

#[tool_router(router = crm_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// `list`: `GET /contacts.json`. `get`: `GET /contacts/{id}.json`.
    /// `create`: `POST /contacts.json`. `update`: `PUT /contacts/{id}.json`.
    /// `delete`: `DELETE /contacts/{id}.json`. `assign_to_project`: `POST
    /// /contacts/{id}/projects.json`. `remove_from_project`: `DELETE
    /// /contacts/{id}/projects/{pid}.json`.
    #[tool(
        description = "List, get, create, update, or delete a RedmineUP CRM contact, or attach/detach one from a project (RedmineUP CRM plugin). list/get work read-only; other actions are blocked. first_name required for create; contact_id required except for list/create; project_id required for create/assign_to_project/remove_from_project. assign_to_project does not create; remove_from_project does not delete.",
        input_schema = crate::tools::schema::input::<ManageContactParams>(),
        output_schema = crate::tools::schema::output::<ManageContactOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn manage_contact(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ManageContactParams>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let boundary = Boundary::new();

        match params.action {
            ManageContactAction::List => {
                let project_id = params
                    .project_id
                    .clone()
                    .map(resolve_project_ref)
                    .transpose()?;
                let limit = clamp_limit(params.limit);
                let offset = params.offset.unwrap_or(0);
                let q = ContactQuery {
                    project_id,
                    search: params.search.clone(),
                    tags: params.tags.clone(),
                    assigned_to_id: params.assigned_to_id,
                };
                let page = match scoped.list_contacts(&q, limit, offset).await {
                    Ok(page) => page,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                let pagination = Pagination::from_page(&page);
                let contacts = page
                    .items
                    .iter()
                    .map(|c| contact_out(&boundary, c))
                    .collect();
                Ok(output::ok(
                    &ManageContactOutput {
                        success: true,
                        contacts: Some(contacts),
                        pagination: Some(pagination),
                        contact: None,
                        updated_fields: None,
                        deleted_contact_id: None,
                        message: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageContactAction::Get => {
                let contact_id = require_contact_id(&params, ManageContactAction::Get)?;
                let includes: Vec<ContactInclude> = params
                    .include
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .map(Into::into)
                    .collect();
                let contact = match scoped.get_contact(ContactId(contact_id), &includes).await {
                    Ok(contact) => contact,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageContactOutput {
                        success: true,
                        contacts: None,
                        pagination: None,
                        contact: Some(contact_out(&boundary, &contact)),
                        updated_fields: None,
                        deleted_contact_id: None,
                        message: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageContactAction::Create => {
                if self.inner.config.read_only {
                    return Ok(read_only_refusal(params.action));
                }
                let first_name = params.first_name.clone().ok_or_else(|| {
                    McpError::invalid_params("first_name is required for action=\"create\"", None)
                })?;
                if first_name.trim().is_empty() {
                    return Err(McpError::invalid_params(
                        "first_name must not be blank",
                        None,
                    ));
                }
                validate_visibility(params.visibility)?;
                let project_id = require_project_ident(&params, ManageContactAction::Create)?;

                let new = ContactWrite {
                    first_name: Some(first_name),
                    last_name: params.last_name.clone(),
                    middle_name: params.middle_name.clone(),
                    company: params.company.clone(),
                    job_title: params.job_title.clone(),
                    phone: params.phone.clone(),
                    email: params.email.clone(),
                    website: params.website.clone(),
                    skype_name: params.skype_name.clone(),
                    birthday: params.birthday.clone(),
                    background: params.background.clone(),
                    address_attributes: params.address.clone().map(Into::into),
                    is_company: params.is_company,
                    visibility: params.visibility,
                    assigned_to_id: params.assigned_to_id,
                    project_id: Some(project_id),
                };
                let contact = match scoped.create_contact(&new).await {
                    Ok(contact) => contact,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageContactOutput {
                        success: true,
                        contacts: None,
                        pagination: None,
                        contact: Some(contact_out(&boundary, &contact)),
                        updated_fields: None,
                        deleted_contact_id: None,
                        message: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageContactAction::Update => {
                if self.inner.config.read_only {
                    return Ok(read_only_refusal(params.action));
                }
                let contact_id = require_contact_id(&params, ManageContactAction::Update)?;
                if let Some(first_name) = &params.first_name
                    && first_name.trim().is_empty()
                {
                    return Err(McpError::invalid_params(
                        "first_name must not be blank if given",
                        None,
                    ));
                }
                validate_visibility(params.visibility)?;

                let mut updated_fields: Vec<&'static str> = Vec::new();
                macro_rules! track {
                    ($field:ident) => {
                        if params.$field.is_some() {
                            updated_fields.push(stringify!($field));
                        }
                    };
                }
                track!(first_name);
                track!(last_name);
                track!(middle_name);
                track!(company);
                track!(job_title);
                track!(phone);
                track!(email);
                track!(website);
                track!(skype_name);
                track!(birthday);
                track!(background);
                track!(address);
                track!(is_company);
                track!(visibility);
                track!(assigned_to_id);
                if updated_fields.is_empty() {
                    return Err(McpError::invalid_params(
                        "at least one field to update is required",
                        None,
                    ));
                }

                let patch = ContactWrite {
                    first_name: params.first_name.clone(),
                    last_name: params.last_name.clone(),
                    middle_name: params.middle_name.clone(),
                    company: params.company.clone(),
                    job_title: params.job_title.clone(),
                    phone: params.phone.clone(),
                    email: params.email.clone(),
                    website: params.website.clone(),
                    skype_name: params.skype_name.clone(),
                    birthday: params.birthday.clone(),
                    background: params.background.clone(),
                    address_attributes: params.address.clone().map(Into::into),
                    is_company: params.is_company,
                    visibility: params.visibility,
                    assigned_to_id: params.assigned_to_id,
                    project_id: None,
                };
                let contact = match scoped.update_contact(ContactId(contact_id), &patch).await {
                    Ok(contact) => contact,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageContactOutput {
                        success: true,
                        contacts: None,
                        pagination: None,
                        contact: Some(contact_out(&boundary, &contact)),
                        updated_fields: Some(updated_fields),
                        deleted_contact_id: None,
                        message: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageContactAction::Delete => {
                if self.inner.config.read_only {
                    return Ok(read_only_refusal(params.action));
                }
                let contact_id = require_contact_id(&params, ManageContactAction::Delete)?;
                if let Err(e) = scoped.delete_contact(ContactId(contact_id)).await {
                    return Ok(to_tool_error(e));
                }
                Ok(output::ok(
                    &ManageContactOutput {
                        success: true,
                        contacts: None,
                        pagination: None,
                        contact: None,
                        updated_fields: None,
                        deleted_contact_id: Some(contact_id),
                        message: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageContactAction::AssignToProject => {
                if self.inner.config.read_only {
                    return Ok(read_only_refusal(params.action));
                }
                let contact_id = require_contact_id(&params, ManageContactAction::AssignToProject)?;
                let project_id =
                    require_project_ident(&params, ManageContactAction::AssignToProject)?;
                if let Err(e) = scoped
                    .assign_contact_to_project(ContactId(contact_id), &project_id)
                    .await
                {
                    return Ok(to_tool_error(e));
                }
                Ok(output::ok(
                    &ManageContactOutput {
                        success: true,
                        contacts: None,
                        pagination: None,
                        contact: None,
                        updated_fields: None,
                        deleted_contact_id: None,
                        message: Some(
                            "the contact was associated with the project; it was not created by this call"
                                .to_string(),
                        ),
                    },
                    self.output_caps(),
                ))
            }
            ManageContactAction::RemoveFromProject => {
                if self.inner.config.read_only {
                    return Ok(read_only_refusal(params.action));
                }
                let contact_id =
                    require_contact_id(&params, ManageContactAction::RemoveFromProject)?;
                let project_id =
                    require_project_ident(&params, ManageContactAction::RemoveFromProject)?;
                if let Err(e) = scoped
                    .remove_contact_from_project(ContactId(contact_id), &project_id)
                    .await
                {
                    return Ok(to_tool_error(e));
                }
                Ok(output::ok(
                    &ManageContactOutput {
                        success: true,
                        contacts: None,
                        pagination: None,
                        contact: None,
                        updated_fields: None,
                        deleted_contact_id: None,
                        message: Some(
                            "the contact's association with the project was removed; the \
                             contact itself was not deleted"
                                .to_string(),
                        ),
                    },
                    self.output_caps(),
                ))
            }
        }
    }
}

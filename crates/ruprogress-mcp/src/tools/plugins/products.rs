//! `manage_product` (`RedmineUP` Products plugin). Registered only when
//! `REDMINE_PRODUCTS_ENABLED=true` — see `server.rs`'s `PLUGIN_TOOLS` gating
//! table.
//!
//! The plugin's wire shapes here are synthetic, derived from the reference
//! implementation's handling of the plugin rather than a live capture
//! (Products is commercial) — see
//! `crates/redmine-client/tests/fixtures/README.md`'s plugin fixtures
//! section. Parameters are flat and typed rather than the reference's
//! untyped `fields` dict (P4/R2): an unknown key is rejected, not silently
//! dropped.

use chrono::{DateTime, Utc};
use redmine_client::ProductId;
use redmine_client::model::custom_field::{CustomFieldValue, CustomFieldWrite};
use redmine_client::model::plugins::products::{Product, ProductQuery, ProductWrite};
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
use crate::tools::issues::{CustomFieldValueOut, custom_field_value_out};
use crate::tools::output::{self, ErrorCode, Pagination};

const LIST_MIN_LIMIT: u32 = 1;
const LIST_MAX_LIMIT: u32 = 100;
const LIST_DEFAULT_LIMIT: u32 = 100;

fn clamp_limit(limit: Option<u32>) -> u32 {
    limit
        .unwrap_or(LIST_DEFAULT_LIMIT)
        .clamp(LIST_MIN_LIMIT, LIST_MAX_LIMIT)
}

// --- custom_fields write input (R1): the shared write-side custom-field
// shape, first needed here; 7f reuses both `CustomFieldValue`'s
// `Serialize` impl and this tool-layer input struct for issues, adding name
// resolution on top. Values are accepted by id only — there is no
// discovery tool for product custom-field definitions to resolve a name
// against. ---

/// A custom field value as given by the caller: a single string, or an
/// array of strings for a `multiple = true` field. Mirrors how Redmine
/// itself represents the value on the wire (see
/// `redmine_client::model::custom_field::CustomFieldValue`'s doc comment);
/// unlike that type this is a tool-input shape the caller controls
/// directly, so `#[serde(untagged)]` is the right tool here (same pattern
/// as `tools::discovery::ProjectRef`).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub(crate) enum CustomFieldValueInput {
    Single(String),
    Multiple(Vec<String>),
}

impl From<CustomFieldValueInput> for CustomFieldValue {
    fn from(v: CustomFieldValueInput) -> Self {
        match v {
            CustomFieldValueInput::Single(s) => Self::Single(Some(s)),
            CustomFieldValueInput::Multiple(values) => Self::Multiple(values),
        }
    }
}

/// One entry of a write-side `custom_fields` array.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CustomFieldEntry {
    /// The custom field's id. Values are accepted by id only.
    pub(crate) id: u64,
    /// The value to set.
    pub(crate) value: CustomFieldValueInput,
}

fn custom_field_entries_to_write(entries: Vec<CustomFieldEntry>) -> Vec<CustomFieldWrite> {
    entries
        .into_iter()
        .map(|e| CustomFieldWrite {
            id: e.id,
            value: e.value.into(),
        })
        .collect()
}

// --- shared output shape ---

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ProductOut {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) code: Option<String>,
    pub(crate) price: Option<f64>,
    pub(crate) currency: Option<String>,
    pub(crate) status_id: Option<u8>,
    pub(crate) category_id: Option<u64>,
    pub(crate) project_id: Option<u64>,
    pub(crate) tag_list: Option<Vec<String>>,
    pub(crate) custom_fields: Option<Vec<CustomFieldValueOut>>,
    pub(crate) created_on: Option<DateTime<Utc>>,
    pub(crate) updated_on: Option<DateTime<Utc>>,
}

fn product_out(boundary: &Boundary, p: &Product) -> ProductOut {
    ProductOut {
        id: p.id,
        name: boundary.wrap("product.name", &p.name),
        description: p
            .description
            .as_deref()
            .map(|d| boundary.wrap("product.description", d)),
        code: p.code.as_deref().map(|c| boundary.wrap("product.code", c)),
        price: p.price,
        currency: p.currency.clone(),
        status_id: p.status_id,
        category_id: p.category_id,
        project_id: p.project_id,
        tag_list: p.tag_list.as_ref().map(|tags| {
            tags.iter()
                .map(|t| boundary.wrap("product.tag", t))
                .collect()
        }),
        custom_fields: p.custom_fields.as_ref().map(|cfs| {
            cfs.iter()
                .map(|cf| custom_field_value_out(boundary, cf))
                .collect()
        }),
        created_on: p.created_on,
        updated_on: p.updated_on,
    }
}

// --- manage_product ---

/// The Products plugin exposes no delete endpoint — this tool has four
/// actions, not five.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ManageProductAction {
    List,
    Get,
    Create,
    Update,
}

impl ManageProductAction {
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
pub(crate) struct ManageProductParams {
    /// Operation to perform. There is no `delete` action — the Products
    /// plugin exposes no delete endpoint.
    pub(crate) action: ManageProductAction,
    /// For `list`, restrict to this project's products (omit for every
    /// accessible product); accepts a numeric id or a slug identifier. For
    /// `create`, optionally associate the new product with a project — a
    /// numeric id only, since the plugin's own `product.project_id` field
    /// is an integer foreign key.
    #[serde(default)]
    pub(crate) project_id: Option<ProjectRef>,
    /// For `list`, max results per call, clamped to 1-100. Default 100.
    #[serde(default)]
    pub(crate) limit: Option<u32>,
    /// For `list`, pagination offset. Default 0.
    #[serde(default)]
    pub(crate) offset: Option<u64>,
    /// The product to act on. Required for `get` and `update`.
    #[serde(default)]
    pub(crate) product_id: Option<u64>,
    /// The product's display name. Required for `create`.
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// Free-text description. For `create`/`update`.
    #[serde(default)]
    pub(crate) description: Option<String>,
    /// A short product code/SKU. For `create`/`update`.
    #[serde(default)]
    pub(crate) code: Option<String>,
    /// Unit price. For `create`/`update`.
    #[serde(default)]
    pub(crate) price: Option<f64>,
    /// The price's currency, e.g. `"USD"`. For `create`/`update`.
    #[serde(default)]
    pub(crate) currency: Option<String>,
    /// `1` = Active, `2` = Inactive. For `create`/`update`. Defaults to
    /// Active on create.
    #[serde(default)]
    pub(crate) status_id: Option<u8>,
    /// The product category id. For `create`/`update`.
    #[serde(default)]
    pub(crate) category_id: Option<u64>,
    /// Replaces the product's full tag set. For `create`/`update`.
    #[serde(default)]
    pub(crate) tag_list: Option<Vec<String>>,
    /// Custom field values to set, by id. For `create`/`update`.
    #[serde(default)]
    pub(crate) custom_fields: Option<Vec<CustomFieldEntry>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ManageProductOutput {
    pub(crate) success: bool,
    /// Populated for `action = "list"`.
    pub(crate) products: Option<Vec<ProductOut>>,
    pub(crate) pagination: Option<Pagination>,
    /// Populated for `action = "get"`/`"create"`/`"update"`.
    pub(crate) product: Option<ProductOut>,
    /// Names of the fields this call changed. `action = "update"` only.
    pub(crate) updated_fields: Option<Vec<&'static str>>,
}

fn read_only_refusal(action: ManageProductAction) -> CallToolResult {
    output::err(
        ErrorCode::ReadOnly,
        format!(
            "this server is running in read-only mode; manage_product(action=\"{}\") is disabled",
            action.as_str()
        ),
        Some(
            "use action=\"list\" or action=\"get\" instead, or ask the operator to disable read-only mode",
        ),
    )
}

fn validate_status_id(status_id: Option<u8>) -> Result<(), McpError> {
    match status_id {
        None | Some(1 | 2) => Ok(()),
        Some(other) => Err(McpError::invalid_params(
            format!("status_id must be 1 (Active) or 2 (Inactive), got {other}"),
            None,
        )),
    }
}

fn require_product_id(
    params: &ManageProductParams,
    action: ManageProductAction,
) -> Result<u64, McpError> {
    params.product_id.ok_or_else(|| {
        McpError::invalid_params(
            format!("product_id is required for action=\"{}\"", action.as_str()),
            None,
        )
    })
}

#[tool_router(router = products_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// `list`: `GET /products.json` or `GET /projects/{pid}/products.json`.
    /// `get`: `GET /products/{id}.json`. `create`: `POST /products.json`.
    /// `update`: `PUT /products/{id}.json`.
    #[tool(
        description = "List, get, create, or update RedmineUP products (RedmineUP Products plugin). There is no delete action. list/get work in read-only mode; create/update are blocked. name is required for create; product_id is required for get/update.",
        input_schema = crate::tools::schema::input::<ManageProductParams>(),
        output_schema = crate::tools::schema::output::<ManageProductOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn manage_product(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ManageProductParams>,
    ) -> Result<CallToolResult, McpError> {
        let scoped = self.scoped(&ctx)?;
        let boundary = Boundary::new();

        match params.action {
            ManageProductAction::List => {
                let project_id = params
                    .project_id
                    .clone()
                    .map(resolve_project_ref)
                    .transpose()?;
                let limit = clamp_limit(params.limit);
                let offset = params.offset.unwrap_or(0);
                let q = ProductQuery { project_id };
                let page = match scoped.list_products(&q, limit, offset).await {
                    Ok(page) => page,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                let pagination = Pagination::from_page(&page);
                let products = page
                    .items
                    .iter()
                    .map(|p| product_out(&boundary, p))
                    .collect();
                Ok(output::ok(
                    &ManageProductOutput {
                        success: true,
                        products: Some(products),
                        pagination: Some(pagination),
                        product: None,
                        updated_fields: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageProductAction::Get => {
                let product_id = require_product_id(&params, ManageProductAction::Get)?;
                let product = match scoped.get_product(ProductId(product_id)).await {
                    Ok(product) => product,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageProductOutput {
                        success: true,
                        products: None,
                        pagination: None,
                        product: Some(product_out(&boundary, &product)),
                        updated_fields: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageProductAction::Create => {
                if self.inner.config.read_only {
                    return Ok(read_only_refusal(params.action));
                }
                let name = params.name.clone().ok_or_else(|| {
                    McpError::invalid_params("name is required for action=\"create\"", None)
                })?;
                if name.trim().is_empty() {
                    return Err(McpError::invalid_params("name must not be blank", None));
                }
                validate_status_id(params.status_id)?;
                let project_id = params
                    .project_id
                    .clone()
                    .map(resolve_project_ref)
                    .transpose()?;
                let new = ProductWrite {
                    name: Some(name),
                    description: params.description.clone(),
                    code: params.code.clone(),
                    price: params.price,
                    currency: params.currency.clone(),
                    status_id: params.status_id,
                    category_id: params.category_id,
                    project_id: match project_id {
                        Some(redmine_client::ProjectIdent::Id(id)) => Some(id.0),
                        Some(redmine_client::ProjectIdent::Identifier(_)) => {
                            return Err(McpError::invalid_params(
                                "project_id must be a numeric project id for manage_product, not a slug identifier",
                                None,
                            ));
                        }
                        None => None,
                    },
                    tag_list: params.tag_list.clone(),
                    custom_fields: params
                        .custom_fields
                        .clone()
                        .map(custom_field_entries_to_write),
                };
                let product = match scoped.create_product(&new).await {
                    Ok(product) => product,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageProductOutput {
                        success: true,
                        products: None,
                        pagination: None,
                        product: Some(product_out(&boundary, &product)),
                        updated_fields: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageProductAction::Update => {
                if self.inner.config.read_only {
                    return Ok(read_only_refusal(params.action));
                }
                let product_id = require_product_id(&params, ManageProductAction::Update)?;
                if let Some(name) = &params.name
                    && name.trim().is_empty()
                {
                    return Err(McpError::invalid_params(
                        "name must not be blank if given",
                        None,
                    ));
                }
                validate_status_id(params.status_id)?;

                let mut updated_fields: Vec<&'static str> = Vec::new();
                if params.name.is_some() {
                    updated_fields.push("name");
                }
                if params.description.is_some() {
                    updated_fields.push("description");
                }
                if params.code.is_some() {
                    updated_fields.push("code");
                }
                if params.price.is_some() {
                    updated_fields.push("price");
                }
                if params.currency.is_some() {
                    updated_fields.push("currency");
                }
                if params.status_id.is_some() {
                    updated_fields.push("status_id");
                }
                if params.category_id.is_some() {
                    updated_fields.push("category_id");
                }
                if params.tag_list.is_some() {
                    updated_fields.push("tag_list");
                }
                if params.custom_fields.is_some() {
                    updated_fields.push("custom_fields");
                }
                if updated_fields.is_empty() {
                    return Err(McpError::invalid_params(
                        "at least one field to update is required",
                        None,
                    ));
                }

                let patch = ProductWrite {
                    name: params.name.clone(),
                    description: params.description.clone(),
                    code: params.code.clone(),
                    price: params.price,
                    currency: params.currency.clone(),
                    status_id: params.status_id,
                    category_id: params.category_id,
                    project_id: None,
                    tag_list: params.tag_list.clone(),
                    custom_fields: params
                        .custom_fields
                        .clone()
                        .map(custom_field_entries_to_write),
                };
                let product = match scoped.update_product(ProductId(product_id), &patch).await {
                    Ok(product) => product,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageProductOutput {
                        success: true,
                        products: None,
                        pagination: None,
                        product: Some(product_out(&boundary, &product)),
                        updated_fields: Some(updated_fields),
                    },
                    self.output_caps(),
                ))
            }
        }
    }
}

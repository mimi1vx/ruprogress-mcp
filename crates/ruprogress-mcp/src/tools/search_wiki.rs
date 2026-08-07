//! Search & Wiki tools: `search_entire_redmine`, `manage_redmine_wiki_page`.
//! See `plans/phase-4e-search-wiki.md`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use redmine_client::WikiTitle;
use redmine_client::model::search::{EntireSearchQuery, SearchResource};
use redmine_client::model::wiki::{WikiPage, WikiPageListItem, WikiPageWrite};
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
use crate::tools::issues::{AttachmentOut, IdNameOut, attachment_out, id_name_out};
use crate::tools::output::{self, ErrorCode, Pagination};

// --- search_entire_redmine ---

const SEARCH_ENTIRE_MIN_LIMIT: u32 = 1;
const SEARCH_ENTIRE_MAX_LIMIT: u32 = 100;
const SEARCH_ENTIRE_DEFAULT_LIMIT: u32 = 100;
/// "First 200 characters", per the reference contract's own wording.
const EXCERPT_MAX_CHARS: usize = 200;

/// Which resource type(s) `search_entire_redmine` restricts to, at the MCP
/// boundary. A closed two-variant enum, not a permissive string list
/// (decision I10): an unrecognized value is an argument-schema failure, not
/// something to silently filter out.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SearchResourceParam {
    Issues,
    WikiPages,
}

impl From<SearchResourceParam> for SearchResource {
    fn from(p: SearchResourceParam) -> Self {
        match p {
            SearchResourceParam::Issues => Self::Issues,
            SearchResourceParam::WikiPages => Self::WikiPages,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchEntireRedmineParams {
    /// Text to search for.
    pub(crate) query: String,
    /// Restrict to these resource types. Defaults to both issues and wiki
    /// pages when omitted.
    #[serde(default)]
    pub(crate) resources: Option<Vec<SearchResourceParam>>,
    /// Maximum results to return, clamped to 1-100. Defaults to 100.
    #[serde(default)]
    pub(crate) limit: Option<u32>,
    /// Pagination offset. Defaults to 0.
    #[serde(default)]
    pub(crate) offset: Option<u64>,
}

fn clamp_search_entire_limit(limit: Option<u32>) -> u32 {
    limit
        .unwrap_or(SEARCH_ENTIRE_DEFAULT_LIMIT)
        .clamp(SEARCH_ENTIRE_MIN_LIMIT, SEARCH_ENTIRE_MAX_LIMIT)
}

/// Truncate `s` to at most [`EXCERPT_MAX_CHARS`] **characters**, not bytes —
/// Redmine sends the full field (there is no server-side truncation to rely
/// on despite the reference contract calling this an "excerpt"; see
/// decision I8).
fn truncate_excerpt(s: &str) -> String {
    if s.chars().count() <= EXCERPT_MAX_CHARS {
        s.to_string()
    } else {
        s.chars().take(EXCERPT_MAX_CHARS).collect()
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct SearchEntireResultOut {
    pub(crate) id: u64,
    /// The resource bucket this result belongs to (`"issues"` or
    /// `"wiki_pages"`) — Redmine's own raw per-result type
    /// (`"issue"`/`"wiki-page"`) re-labelled to match the `resources`
    /// parameter's values, never passed through verbatim (decision I2).
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) title: String,
    /// The first 200 characters of Redmine's match text. `None` when
    /// Redmine sent nothing for this hit. Never the full field: use
    /// `get_redmine_issue`/`manage_redmine_wiki_page(action="get")` for the
    /// complete content (decision I8 — this tool does not hydrate).
    pub(crate) excerpt: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct SearchEntireRedmineOutput {
    pub(crate) results: Vec<SearchEntireResultOut>,
    /// Tally of `results` by bucket, computed over **this page only**.
    /// Redmine's `search.json` exposes no cross-type total (decision I7);
    /// this is not the same as a global count when paging with
    /// `limit`/`offset`.
    pub(crate) results_by_type: BTreeMap<String, u64>,
    pub(crate) pagination: Pagination,
}

// --- manage_redmine_wiki_page ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ManageWikiPageAction {
    List,
    Get,
    Create,
    Update,
    Delete,
    Rename,
}

impl ManageWikiPageAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Get => "get",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Rename => "rename",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManageRedmineWikiPageParams {
    /// Operation to perform.
    pub(crate) action: ManageWikiPageAction,
    /// The project the wiki page belongs to. Required for every action.
    pub(crate) project_id: ProjectRef,
    /// The page title. Required for every action except `list`.
    #[serde(default)]
    pub(crate) wiki_page_title: Option<String>,
    /// Specific revision to fetch. `get` only; defaults to the latest.
    #[serde(default)]
    pub(crate) version: Option<u32>,
    /// Include attachment metadata in the `get` response. Defaults to
    /// `true`; has no effect on any other action.
    #[serde(default)]
    pub(crate) include_attachments: Option<bool>,
    /// Page content. Required for `create` and `update`.
    #[serde(default)]
    pub(crate) text: Option<String>,
    /// Change-log comment for `create`/`update`/`rename`.
    #[serde(default)]
    pub(crate) comments: Option<String>,
    /// The new title. Required for `rename`; must differ from
    /// `wiki_page_title`.
    #[serde(default)]
    pub(crate) new_title: Option<String>,
    /// When `true` (default), `rename` leaves a redirect from the old title
    /// to the new one.
    #[serde(default)]
    pub(crate) redirect_existing_links: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct WikiPageListItemOut {
    pub(crate) title: String,
    pub(crate) parent_title: Option<String>,
    pub(crate) version: u32,
    pub(crate) created_on: DateTime<Utc>,
    pub(crate) updated_on: Option<DateTime<Utc>>,
}

fn wiki_page_list_item_out(item: &WikiPageListItem) -> WikiPageListItemOut {
    WikiPageListItemOut {
        title: item.title.clone(),
        parent_title: item.parent.as_ref().map(|p| p.title.clone()),
        version: item.version,
        created_on: item.created_on,
        updated_on: item.updated_on,
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct WikiPageOut {
    pub(crate) title: String,
    pub(crate) text: Option<String>,
    pub(crate) version: u32,
    pub(crate) author: Option<IdNameOut>,
    pub(crate) comments: Option<String>,
    pub(crate) parent_title: Option<String>,
    /// `None` on Redmine < 7.0, which omits the field entirely (see
    /// `redmine_client::model::wiki::WikiPage::project`'s doc comment).
    pub(crate) project_id: Option<u64>,
    pub(crate) created_on: DateTime<Utc>,
    pub(crate) updated_on: Option<DateTime<Utc>>,
    /// `None` when attachments were not requested; `Some(vec![])` when
    /// requested but empty.
    pub(crate) attachments: Option<Vec<AttachmentOut>>,
}

fn wiki_page_out(boundary: &Boundary, p: &WikiPage) -> WikiPageOut {
    WikiPageOut {
        title: p.title.clone(),
        text: p
            .text
            .as_deref()
            .map(|t| boundary.wrap("wiki_page.text", t)),
        version: p.version,
        author: p
            .author
            .as_ref()
            .map(|u| id_name_out(boundary, "user.name", u)),
        comments: p
            .comments
            .as_deref()
            .map(|c| boundary.wrap("wiki_page.comments", c)),
        parent_title: p.parent.as_ref().map(|pp| pp.title.clone()),
        project_id: p.project.as_ref().map(|pr| pr.id),
        created_on: p.created_on,
        updated_on: p.updated_on,
        attachments: p
            .attachments
            .as_ref()
            .map(|atts| atts.iter().map(|a| attachment_out(boundary, a)).collect()),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ManageWikiPageOutput {
    pub(crate) success: bool,
    /// Populated for `action = "list"`.
    pub(crate) pages: Option<Vec<WikiPageListItemOut>>,
    /// Populated for `action = "get"`/`"create"`/`"update"`/`"rename"`.
    pub(crate) page: Option<WikiPageOut>,
    /// Populated for `action = "delete"`.
    pub(crate) deleted_title: Option<String>,
    /// A human-readable note, currently only set on `delete` to explain
    /// that child pages survive un-parented rather than being removed
    /// (decision I6).
    pub(crate) message: Option<String>,
}

fn resolve_wiki_title(s: &str) -> Result<WikiTitle, McpError> {
    WikiTitle::new(s).map_err(|e| McpError::invalid_params(e.to_string(), None))
}

fn require_title(
    params: &ManageRedmineWikiPageParams,
    action: ManageWikiPageAction,
) -> Result<WikiTitle, McpError> {
    let raw = params.wiki_page_title.clone().ok_or_else(|| {
        McpError::invalid_params(
            format!(
                "wiki_page_title is required for action=\"{}\"",
                action.as_str()
            ),
            None,
        )
    })?;
    resolve_wiki_title(&raw)
}

fn read_only_refusal(action: ManageWikiPageAction) -> CallToolResult {
    output::err(
        ErrorCode::ReadOnly,
        format!(
            "this server is running in read-only mode; manage_redmine_wiki_page(action=\"{}\") is disabled",
            action.as_str()
        ),
        Some(
            "use action=\"list\" or action=\"get\" instead, or ask the operator to disable read-only mode",
        ),
    )
}

#[tool_router(router = search_wiki_tool_router, vis = "pub(crate)")]
impl RedmineMcp {
    /// `GET /search.json?issues=1&wiki_pages=1` (or a subset via
    /// `resources`).
    #[tool(
        description = "Search across issues and wiki pages in one call. Use this for a broad text search when the resource type is not yet known. Prefer search_redmine_issues for issue-only search with richer filtering (scope, open_issues, field selection). Results are thin (id, title, excerpt only) — follow up with get_redmine_issue or manage_redmine_wiki_page(action=\"get\") for full details.",
        input_schema = crate::tools::schema::input::<SearchEntireRedmineParams>(),
        output_schema = crate::tools::schema::output::<SearchEntireRedmineOutput>(),
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true),
    )]
    pub(crate) async fn search_entire_redmine(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<SearchEntireRedmineParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.query.trim().is_empty() {
            return Err(McpError::invalid_params("query must not be empty", None));
        }
        let limit = clamp_search_entire_limit(params.limit);
        let offset = params.offset.unwrap_or(0);
        let resources: Vec<SearchResource> = params.resources.map_or_else(
            || vec![SearchResource::Issues, SearchResource::WikiPages],
            |rs| rs.into_iter().map(Into::into).collect(),
        );
        let query = EntireSearchQuery {
            q: params.query,
            resources,
        };

        let scoped = self.scoped(&ctx)?;
        let page = match scoped.search_entire_page(&query, limit, offset).await {
            Ok(page) => page,
            Err(e) => return Ok(to_tool_error(e)),
        };

        let boundary = Boundary::new();
        let mut results_by_type: BTreeMap<String, u64> = BTreeMap::new();
        let results = page
            .items
            .iter()
            .map(|r| {
                let bucket = SearchResource::from_raw_type(&r.kind)
                    .map_or("unknown", SearchResource::wire_param);
                let counter = results_by_type.entry(bucket.to_string()).or_insert(0);
                *counter = counter.saturating_add(1);
                SearchEntireResultOut {
                    id: r.id,
                    kind: bucket,
                    title: boundary.wrap("search_result.title", &r.title),
                    excerpt: r
                        .description
                        .as_deref()
                        .map(|d| boundary.wrap("search_result.excerpt", &truncate_excerpt(d))),
                }
            })
            .collect();

        let pagination = Pagination::from_page(&page);
        Ok(output::ok(
            &SearchEntireRedmineOutput {
                results,
                results_by_type,
                pagination,
            },
            self.output_caps(),
        ))
    }

    /// `list`: `GET /projects/{id}/wiki/index.json`. `get`: `GET
    /// /projects/{id}/wiki/{title}[/{version}].json`. `create`/`update`/
    /// `rename`: `PUT /projects/{id}/wiki/{title}.json`. `delete`: `DELETE
    /// /projects/{id}/wiki/{title}.json`.
    #[tool(
        description = "List, get, create, update, delete, or rename a wiki page. Use this to manage project documentation. project_id and action are required; wiki_page_title is required except for list. create/update need text; rename needs new_title. list/get work in read-only mode; the rest are blocked. Deleting a page un-parents its children rather than deleting them.",
        input_schema = crate::tools::schema::input::<ManageRedmineWikiPageParams>(),
        output_schema = crate::tools::schema::output::<ManageWikiPageOutput>(),
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        ),
    )]
    pub(crate) async fn manage_redmine_wiki_page(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(params): Parameters<ManageRedmineWikiPageParams>,
    ) -> Result<CallToolResult, McpError> {
        let project_ident = resolve_project_ref(params.project_id.clone())?;
        let scoped = self.scoped(&ctx)?;
        let boundary = Boundary::new();

        match params.action {
            ManageWikiPageAction::List => {
                let pages = match scoped.list_wiki_pages(&project_ident).await {
                    Ok(pages) => pages,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageWikiPageOutput {
                        success: true,
                        pages: Some(pages.iter().map(wiki_page_list_item_out).collect()),
                        page: None,
                        deleted_title: None,
                        message: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageWikiPageAction::Get => {
                let title = require_title(&params, ManageWikiPageAction::Get)?;
                let include_attachments = params.include_attachments.unwrap_or(true);
                let page = match scoped
                    .get_wiki_page(&project_ident, &title, params.version, include_attachments)
                    .await
                {
                    Ok(page) => page,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageWikiPageOutput {
                        success: true,
                        pages: None,
                        page: Some(wiki_page_out(&boundary, &page)),
                        deleted_title: None,
                        message: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageWikiPageAction::Create | ManageWikiPageAction::Update => {
                if self.inner.config.read_only {
                    return Ok(read_only_refusal(params.action));
                }
                let title = require_title(&params, params.action)?;
                let text = params.text.clone().ok_or_else(|| {
                    McpError::invalid_params(
                        format!("text is required for action=\"{}\"", params.action.as_str()),
                        None,
                    )
                })?;
                let write = WikiPageWrite {
                    text,
                    comments: params.comments.clone(),
                    ..WikiPageWrite::default()
                };
                let page = match scoped
                    .upsert_wiki_page(&project_ident, &title, &write)
                    .await
                {
                    Ok(page) => page,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageWikiPageOutput {
                        success: true,
                        pages: None,
                        page: Some(wiki_page_out(&boundary, &page)),
                        deleted_title: None,
                        message: None,
                    },
                    self.output_caps(),
                ))
            }
            ManageWikiPageAction::Delete => {
                if self.inner.config.read_only {
                    return Ok(read_only_refusal(params.action));
                }
                let title = require_title(&params, ManageWikiPageAction::Delete)?;
                if let Err(e) = scoped.delete_wiki_page(&project_ident, &title).await {
                    return Ok(to_tool_error(e));
                }
                Ok(output::ok(
                    &ManageWikiPageOutput {
                        success: true,
                        pages: None,
                        page: None,
                        deleted_title: Some(title.as_str().to_string()),
                        message: Some(
                            "wiki page deleted; any child pages were un-parented, not deleted"
                                .to_string(),
                        ),
                    },
                    self.output_caps(),
                ))
            }
            ManageWikiPageAction::Rename => {
                if self.inner.config.read_only {
                    return Ok(read_only_refusal(params.action));
                }
                let old_title = require_title(&params, ManageWikiPageAction::Rename)?;
                let new_title_raw = params.new_title.clone().ok_or_else(|| {
                    McpError::invalid_params("new_title is required for action=\"rename\"", None)
                })?;
                if new_title_raw == old_title.as_str() {
                    return Err(McpError::invalid_params(
                        "new_title must differ from wiki_page_title",
                        None,
                    ));
                }
                let new_title = resolve_wiki_title(&new_title_raw)?;

                let current = match scoped
                    .get_wiki_page(&project_ident, &old_title, None, false)
                    .await
                {
                    Ok(page) => page,
                    Err(e) => return Ok(to_tool_error(e)),
                };
                let redirect_existing_links =
                    (params.redirect_existing_links == Some(false)).then_some("0");
                let write = WikiPageWrite {
                    text: current.text.unwrap_or_default(),
                    comments: params.comments.clone(),
                    title: Some(new_title_raw),
                    redirect_existing_links,
                };
                if let Err(e) = scoped
                    .write_wiki_page(&project_ident, &old_title, &write)
                    .await
                {
                    return Ok(to_tool_error(e));
                }

                let renamed = match scoped
                    .get_wiki_page(&project_ident, &new_title, None, false)
                    .await
                {
                    Ok(page) => page,
                    Err(redmine_client::Error::NotFound) => {
                        return Ok(output::err(
                            ErrorCode::Forbidden,
                            "the wiki page was updated but its title did not change; the configured credential likely lacks the rename_wiki_pages permission",
                            Some(
                                "ask the user for an account with rename_wiki_pages, or update the page's text without attempting a rename",
                            ),
                        ));
                    }
                    Err(e) => return Ok(to_tool_error(e)),
                };
                Ok(output::ok(
                    &ManageWikiPageOutput {
                        success: true,
                        pages: None,
                        page: Some(wiki_page_out(&boundary, &renamed)),
                        deleted_title: None,
                        message: None,
                    },
                    self.output_caps(),
                ))
            }
        }
    }
}

# Tool contract (vendored from upstream reference server)

Source: [`jztan/redmine-mcp-server`](https://github.com/jztan/redmine-mcp-server),
branch `develop` (`main` 404s), `docs/tool-reference.md`, captured 2026-08-06
at commit-adjacent content (52 `###`-level tool sections found).

For each tool: name, parameter names + types + required-ness (as given upstream),
and a one-line return shape. This is a snapshot for drift detection, not a spec —
the upstream doc is authoritative; re-vendor when implementing more tools.

Tools currently implemented (all 36 non-app core tools from
`plans/phase-4-core-tools.md`; kept in sync with `IMPLEMENTED_TOOLS` in
`crates/ruprogress-mcp/tests/tools_basic.rs`, which fails CI on drift):
`get_mcp_server_info`, `get_current_user`, `list_redmine_projects`,
`list_redmine_trackers`, `list_project_trackers`,
`list_redmine_issue_statuses`, `list_redmine_issue_priorities`,
`list_redmine_users`, `list_redmine_queries`,
`list_project_issue_custom_fields`, `summarize_project_status`,
`list_redmine_versions`, `manage_redmine_version`, `list_project_members`,
`list_redmine_roles`, `get_project_modules`, `manage_project_member`,
`get_redmine_issue`, `list_redmine_issues`, `search_redmine_issues`,
`list_subtasks`, `get_private_notes`, `list_time_entries`,
`manage_time_entry`, `list_time_entry_activities`, `import_time_entries`,
`create_redmine_issue`, `update_redmine_issue`, `delete_redmine_issue`,
`copy_issue`, `manage_issue_relation`, `manage_issue_watcher`,
`manage_issue_note`, `manage_issue_category`, `search_entire_redmine`,
`manage_redmine_wiki_page`, `get_gantt_chart`, `get_redmine_attachment`
(`list_files`/`upload_file`/`delete_file`/`cleanup_attachment_files` remain
unimplemented). All remaining sections below (MCP Apps, the rest of File
Operations, Checklist Tools, Products/Contacts/Documents plugin families)
are recorded for reference as app-plugin tools out of Phase 4's scope (see
the parent plan's "non-app core tools" framing).

## Project Management

### `list_redmine_projects`

Parameters: none

Returns: List of project dictionaries with id, name, identifier, and description

### `list_project_issue_custom_fields`

Parameters:
- `project_id` (integer or string, required): Project ID (numeric) or identifier (string)
- `tracker_id` (integer, optional): Restrict output to fields applicable to the given tracker ID

Returns: List of custom field metadata dictionaries

### `summarize_project_status`

Parameters:
- `project_id` (integer, required): The ID of the project to summarize
- `days` (integer, optional): Number of days to analyze. Default: `30`

Returns: Comprehensive project status summary including:

### `list_redmine_versions`

Parameters:
- `project_id` (integer or string, required): The project ID (numeric) or identifier (string)
- `status_filter` (string, optional): Filter by version status. Allowed values: `open`, `locked`, `closed`. Default: all versions

Returns: List of version dictionaries

### `manage_redmine_version`

Parameters:
- `action` (string, required): Operation to perform. Allowed values: `create`, `update`, `delete`
- `project_id` (integer or string): Project ID or identifier. Required for `action="create"`
- `version_id` (integer): Version ID. Required for `action="update"` and `action="delete"`
- `name` (string): Version name. Required for `action="create"`, optional for `action="update"`
- `description` (string, optional): Version description
- `status` (string, optional): Version status. Allowed values: `open`, `locked`, `closed`. Defaults to `open` on create
- `due_date` (string, optional): Due date in `YYYY-MM-DD` format
- `sharing` (string, optional): Sharing scope. Allowed values: `none`, `descendants`, `hierarchy`, `tree`, `system`. Defaults to `none` on create
- `wiki_page_title` (string, optional): Associated wiki page title

Returns: - `create`/`update`: full version dictionary (same shape as `list_redmine_versions` entries)

### `list_project_members`

Parameters:
- `project_id` (integer or string, required): Project ID (numeric) or identifier (string)

Returns: List of membership dictionaries containing user/group info and roles

### `list_redmine_roles`

Parameters: none

Returns: List of role dictionaries with `id` and `name`.

### `get_project_modules`

Parameters:
- `project_id` (integer or string, required): Project identifier (numeric ID or string identifier).

Returns: Dictionary with `project_id`, `project_name`, and `enabled_modules` (list of module name strings).

### `manage_project_member`

Parameters:
- `action` (string, required): Operation to perform. Allowed: `add`, `update`, `remove`
- `project_id` (integer or string): Project identifier. Required for `action="add"`
- `membership_id` (integer): Membership ID. Required for `action="update"` and `action="remove"`
- `user_id` (integer): ID of the user. Exactly one of `user_id` or `group_id` required for `action="add"`
- `group_id` (integer): ID of the group. Exactly one of `user_id` or `group_id` required for `action="add"`
- `role_ids` (array of integers): Non-empty list of role IDs. Required for `action="add"` and `action="update"`. Use `list_redmine_roles` to discover valid IDs

Returns: - `add`/`update`: membership dictionary (with `id`, `user`/`group`, `project`, `roles`)

## Issue Operations

### `get_redmine_issue`

Parameters:
- `issue_id` (integer, required): The ID of the issue to retrieve
- `include_journals` (boolean, optional): Include journals (comments) in result. Default: `true`
- `include_attachments` (boolean, optional): Include attachments metadata. Default: `true`
- `include_custom_fields` (boolean, optional): Include custom fields in result. Default: `true`
- `journal_limit` (integer, optional): Maximum number of journals to return. When set, enables journal pagination and adds `journal_pagination` metadata. Default: `null` (all journals)
- `journal_offset` (integer, optional): Number of journals to skip (used with `journal_limit`). Default: `0`
- `include_watchers` (boolean, optional): Include watcher list. Default: `false`
- `include_relations` (boolean, optional): Include issue relations. Default: `false`
- `include_children` (boolean, optional): Include child issues. Default: `false`

Returns: Issue dictionary with details, journals, and attachments. Standard fields include `category`, `fixed_version` (target version), and `parent` (each `{id, ...}` or `None`), plus `start_date`, `due_date`, `closed_on` (ISO-8601 or `None`), `done_ratio`, `estimated_hours`, `spent_hours`, and `is_private`. Each is `None` when not set on the issue. When `REDMINE_AGILE_ENABLED=true`, also includes `story_points`, `agile_sprint_id`, and `agile_position` from the RedmineUP Agile plugin.

### `list_redmine_issues`

Parameters:
- `project_id` (integer or string, optional): Filter by project (numeric ID or string identifier)
- `status_id` (integer, optional): Filter by status ID
- `tracker_id` (integer, optional): Filter by tracker ID
- `assigned_to_id` (integer or string, optional): Filter by assignee. Use a numeric user ID or the special value `'me'` to retrieve issues assigned to the currently authenticated user. Note that `'me'` resolves to the owner of the configured `REDMINE_API_KEY`, which may be a shared or robot account rather than the human operator. If results come back unexpectedly empty, call [`get_mcp_server_info`](#get_mcp_server_info) to confirm who `'me'` maps to.
- `priority_id` (integer, optional): Filter by priority ID
- `fixed_version_id` (integer, optional): Filter by target version/milestone ID
- `sort` (string, optional): Sort order (e.g., `"updated_on:desc"`)
- `limit` (integer, optional): Maximum issues to return. Default: `25`, Max: `1000`
- `offset` (integer, optional): Number of issues to skip for pagination. Default: `0`
- `include_pagination_info` (boolean, optional): Return structured response with metadata. Default: `false`
- `fields` (array of strings, optional): List of field names to include in results. Default: all fields Available fields: `id`, `subject`, `description`, `project`, `status`, `priority`, `tracker`, `author`, `assigned_to`, `created_on`, `updated_on` — `tracker` is returned by default Special values: `["*"]` or `["all"]` for all fields

Returns: List of issue dictionaries, or structured response with pagination metadata

### `search_redmine_issues`

Parameters:
- `query` (string, required): Text to search for in issues
- `limit` (integer, optional): Maximum number of issues to return. Default: `25`, Max: `1000`
- `offset` (integer, optional): Number of issues to skip for pagination. Default: `0`
- `include_pagination_info` (boolean, optional): Return structured response with pagination metadata. Default: `false`
- `fields` (array of strings, optional): List of field names to include in results. Default: `null` (all fields) Available fields: `id`, `subject`, `description`, `project`, `status`, `priority`, `tracker`, `author`, `assigned_to`, `created_on`, `updated_on` — `tracker` is returned by default Special values: `["*"]` or `["all"]` for all fields
- `scope` (string, optional): Search scope. Default: `"all"` Values: `"all"`, `"my_project"`, `"subprojects"`
- `open_issues` (boolean, optional): Search only open issues. Default: `false`

Returns: - By default: List of issue dictionaries

### `create_redmine_issue`

Parameters:
- `project_id` (integer, required): Target project ID
- `subject` (string, required): Issue subject/title
- `description` (string, optional): Issue description. Default: `""`
- `fields` (object|string, optional): Additional Redmine fields as: an object (`{"priority_id": 3, "tracker_id": 1}`), or a serialized JSON object string (for MCP clients that pass string payloads)
- `extra_fields` (object|string, optional): Additional Redmine fields as: an object (`{"priority_id": 3, "tracker_id": 1}`), or a serialized JSON object string
- `uploads` (list, optional): Files to attach to the issue. Maximum 10 items. Each item is an object with: Exactly ONE source key: `content_base64` (string): Raw file bytes encoded as base64. `filename` is required when using this source. `source_url` (string): HTTP(S) URL the server fetches. Filename is derived from the URL or `Content-Disposition` if omitted. `file_path` (string): Absolute path to a file already on the server. Must be inside `ATTACHMENTS_DIR` or a directory listed in `REDMINE_MCP_UPLOAD_FILE_ROOTS`. Filename is derived from the path if omitted. `filename` (string, optional): Name the attachment will have in Redmine. Required for `content_base64`; derived for other sources when omitted. `content_type` (string, optional): MIME type override (e.g. `"application/pdf"`). `description` (string, optional): Human-readable description for the attachment.

Returns: Created issue dictionary. When `uploads` is provided and at least one attachment succeeds, the response includes:

### `update_redmine_issue`

Parameters:
- `issue_id` (integer, required): ID of the issue to update
- `fields` (object, required): Dictionary of fields to update
- `uploads` (list, optional): Files to attach to the issue. Maximum 10 items. Each item is an object with: Exactly ONE source key: `content_base64` (string): Raw file bytes encoded as base64. `filename` is required when using this source. `source_url` (string): HTTP(S) URL the server fetches. Filename is derived from the URL or `Content-Disposition` if omitted. `file_path` (string): Absolute path to a file already on the server. Must be inside `ATTACHMENTS_DIR` or a directory listed in `REDMINE_MCP_UPLOAD_FILE_ROOTS`. Filename is derived from the path if omitted. `filename` (string, optional): Name the attachment will have in Redmine. Required for `content_base64`; derived for other sources when omitted. `content_type` (string, optional): MIME type override (e.g. `"application/pdf"`). `description` (string, optional): Human-readable description for the attachment.

Returns: Updated issue dictionary. When `uploads` is provided and at least one attachment succeeds, the response includes:

### `delete_redmine_issue`

Parameters:
- `issue_id` (integer, required): ID of the issue to delete. Must be a positive integer.
- `confirm_delete` (boolean, optional): When `False` (default), the tool refuses and returns an impact preview. Pass `True` to actually delete.
- `confirm_delete_with_children` (boolean, optional): When the issue has subtasks, `confirm_delete=True` alone refuses with code `CHILDREN_PRESENT`. Pass this flag too to opt in to cascade-deleting the subtasks.

Returns: Refusal envelope `{error, code, hint, impact}` (default, or when subtasks present) or success envelope `{success, deleted_issue_id, cascade_deleted}`. Write tool; requires `confirm_delete=true` (and `confirm_delete_with_children=true` if the issue has subtasks).

### `copy_issue`

Parameters:
- `issue_id` (integer, required): ID of the source issue to copy.
- `project_id` (integer or string, optional): Target project for the copy. Defaults to the source issue's project.
- `subject` (string, optional): New subject for the copy. Defaults to the source subject.
- `link_original` (boolean, optional): Create a `copied_to`/`copied_from` relation between the original and the copy. Default: `true`.
- `copy_subtasks` (boolean, optional): Recursively copy the source's subtasks. Default: `true`.
- `copy_attachments` (boolean, optional): Copy attachments to the new issue. Default: `true`.
- `field_overrides` (object or JSON string, optional): Field values to override on the copy (e.g., `{"assigned_to_id": 5, "description": "..."}`).

Returns: Dictionary containing the newly created issue. On failure, a dict with an `"error"` key.

### `manage_issue_relation`

Parameters:
- `action` (string, required): Operation to perform. Allowed: `list`, `create`, `delete`
- `issue_id` (integer): Source issue ID. Required for `action="list"` and `action="create"`
- `issue_to_id` (integer): Target issue ID. Required for `action="create"`
- `relation_id` (integer): Relation ID. Required for `action="delete"`
- `relation_type` (string, optional): One of `relates`, `duplicates`, `duplicated`, `blocks`, `blocked`, `precedes`, `follows`, `copied_to`, `copied_from`. Defaults to `relates` on create
- `delay` (integer, optional): Delay in days. Only meaningful for `precedes` / `follows`

Returns: - `list`: array of relation dicts (`id`, `issue_id`, `issue_to_id`, `relation_type`, `delay`)

### `list_subtasks`

Parameters:
- `issue_id` (integer, required): ID of the parent issue.

Returns: List of child issue dictionaries.

### `manage_issue_watcher`

Parameters:
- `action` (string, required): Allowed: `add`, `remove`
- `issue_id` (integer, required): ID of the issue
- `user_id` (integer, required): ID of the user to add or remove

Returns: `{"success": true, "issue_id": ..., "user_id": ...}` on success; `{"error": "..."}` on failure.

### `manage_issue_note`

Parameters:
- `action` (string, required): Allowed: `edit`, `set_private`
- `journal_id` (integer, required): ID of the journal entry (from `get_redmine_issue` with `include_journals=true`)
- `notes` (string): New notes text (may be empty to clear). Required for `action="edit"`
- `private_notes` (boolean, optional): Optionally toggle the private flag during `edit`
- `is_private` (boolean): Required for `action="set_private"` — `true` to mark private, `false` to make public

Returns: - `edit`: `{"success": true, "journal_id": ..., "notes": ..., "private_notes": ...}`

### `get_private_notes`

Parameters:
- `issue_id` (integer, required): ID of the issue.

Returns: List of journal dictionaries where `private_notes` is `true`. Journals with empty note bodies are omitted.

### `manage_issue_category`

Parameters:
- `action` (string, required): Allowed: `list`, `create`, `update`, `delete`
- `project_id` (integer or string): Project identifier. Required for `action="list"` and `action="create"`
- `category_id` (integer): Category ID. Required for `action="update"` and `action="delete"`
- `name` (string): Category name. Required for `action="create"`, optional for `action="update"` (cannot be blank)
- `assigned_to_id` (integer, optional): Default assignee user ID. For `create` and `update`
- `reassign_to_id` (integer, optional): Reassign existing issues to this category ID on `delete`. If omitted, issues become uncategorised

Returns: - `list`: array of category dicts (`id`, `name`, `project`, `assigned_to`)

## MCP Apps (Interactive Tools)

### `show_triage_board`

Parameters:
- `project_id` (int | str, required): project to display.
- `filters` (dict, optional): extra Redmine filters, same as `list_redmine_issues`.

Returns: MCP Apps UI resource (interactive Kanban board); no plain-JSON return.

### `get_triage_board_data`

Parameters:
- same parameters as `show_triage_board`

Returns: Same JSON payload as `show_triage_board`, without the UI resource (used by the board iframe's Refresh action).

### `show_project_dashboard`

Parameters:
- `project_id` (int | str, required): project to display.
- `filters` (dict, optional): extra Redmine filters, same as `list_redmine_issues`.

Returns: MCP Apps UI resource (interactive project dashboard); no plain-JSON return.

### `get_project_dashboard_data`

Parameters:
- same parameters as `show_project_dashboard`

Returns: Same JSON payload as `show_project_dashboard`, without the UI resource (used by the dashboard iframe's Refresh action).

## Time Tracking

### `list_time_entries`

Parameters:
- `project_id` (integer or string, optional): Filter by project (numeric ID or string identifier)
- `issue_id` (integer, optional): Filter by issue ID
- `user_id` (integer or string, optional): Filter by user ID. Use `"me"` for current user
- `from_date` (string, optional): Start date filter (YYYY-MM-DD format)
- `to_date` (string, optional): End date filter (YYYY-MM-DD format)
- `limit` (integer, optional): Maximum entries to return. Default: `25`, Max: `100`
- `offset` (integer, optional): Number of entries to skip for pagination. Default: `0`

Returns: List of time entry dictionaries

### `manage_time_entry`

Parameters:
- `action` (string, required): Allowed: `create`, `update`
- `hours` (float): Hours spent. Required for `action="create"`; optional for `update` (must be positive if provided)
- `project_id` (integer or string): Required for `action="create"` if `issue_id` is not provided
- `issue_id` (integer): Required for `action="create"` if `project_id` is not provided
- `user_id` (integer, optional): Log on behalf of this user (`create` only). Requires `log_time_for_other_users` permission
- `time_entry_id` (integer): Entry ID to update. Required for `action="update"`
- `activity_id` (integer, optional): Activity type (e.g., Development, Design)
- `comments` (string, optional): Description. Empty string clears the field on `update`
- `spent_on` (string, optional): Date in `YYYY-MM-DD` format

Returns: - `create`/`update`: time entry dict (`id`, `hours`, `comments`, `spent_on`, `user`, `project`, `issue`, `activity`, etc.)

### `list_time_entry_activities`

Parameters:
- `project_id` (string or integer, optional): Project identifier. When provided, returns project-specific activities via `GET /projects/:id.json?include=time_entry_activities` (Redmine 3.4.0+).

Returns: - Without `project_id`: list of activity dicts with `id`, `name`, `active`, `is_default`

## Discovery / Enumeration Tools

### `list_redmine_trackers`

Parameters: none

Returns: List of `{id, name, description}` dicts.

### `list_project_trackers`

Parameters:
- `project_id` (integer or string, required): Project ID (numeric) or identifier (string)

Returns: List of `{id, name}` dicts for trackers enabled on the project.

### `list_redmine_issue_statuses`

Parameters: none

Returns: List of `{id, name, is_closed}` dicts — `is_closed` flags statuses that count as "closed" for reporting purposes.

### `list_redmine_issue_priorities`

Parameters: none

Returns: List of `{id, name, active, is_default}` dicts.

### `list_redmine_users`

Parameters:
- `name` (string, optional): Case-insensitive substring filter (matches login, firstname, lastname, email).
- `group_id` (integer, optional): Filter users who belong to a specific group.
- `limit` (integer, optional): Maximum users to return (default 25, clamped to 1–100).
- `offset` (integer, optional): Pagination offset. Default 0.

Returns: List of `{id, login, firstname, lastname, mail, created_on}` dicts.

### `get_current_user`

Parameters: none

Returns: Dict with `id, login, firstname, lastname, mail, admin, created_on, last_login_on`.

### `list_redmine_queries`

Parameters: none

Returns: List of `{id, name, is_public, project_id}` dicts. `project_id` is `null` for cross-project queries.

### `import_time_entries`

Parameters:
- `entries` (array of objects, required): List of time entry dicts. Each entry accepts: `hours` (required), plus at least one of `project_id`/`issue_id`. Optional: `user_id` (log on behalf of a teammate), `activity_id`, `comments`, `spent_on`. Capped at 500 entries per call -- split larger imports into multiple invocations. (The JSON-string variant was dropped in #114; passing a string is rejected at the FastMCP boundary with the `INVALID_ARGUMENTS` envelope.)
- `stop_on_error` (boolean, optional): Abort on the first error. Default: `false` (continue past errors).

Returns: Dictionary with:

## Search & Wiki

### `search_entire_redmine`

Parameters:
- `query` (string, required): Text to search for
- `resources` (list, optional): Filter by resource types. Allowed: `["issues", "wiki_pages"]`. Default: both types
- `limit` (integer, optional): Maximum results to return (max 100). Default: 100
- `offset` (integer, optional): Pagination offset. Default: 0

Returns: ```json

### `manage_redmine_wiki_page`

Parameters:
- `action` (string, required): Allowed: `list`, `get`, `create`, `update`, `delete`, `rename`
- `project_id` (integer or string, required): Project identifier (numeric ID or short name)
- `wiki_page_title` (string): Wiki page title. Required for all actions except `list`
- `version` (integer, optional): Specific version number for `get` (default: latest)
- `include_attachments` (boolean, optional): Include attachment metadata in `get` response. Default: `true`
- `text` (string): Page content. Required for `create` and `update`
- `comments` (string, optional): Change log comment for `create` and `update`
- `new_title` (string): New title for `rename` (must differ from `wiki_page_title`)
- `redirect_existing_links` (boolean, optional): When `true` (default), `rename` creates a `WikiRedirect` from the old title to the new title

Returns: - `list`: array of page metadata dicts (`title`, `version`, `parent_title` if present, `created_on`, `updated_on`) — no body text

## File Operations

### `list_files`

Parameters:
- `project_id` (integer or string, required): Project identifier.

Returns: List of file metadata dictionaries (`id`, `filename`, `filesize`, `content_type`, `description`, `content_url`, `digest`, `downloads`, `author`, `version`, `created_on`).

### `upload_file`

Parameters:
- `project_id` (integer or string, required): Project identifier.
- `filename` (string, optional): Name the file should have in Redmine. Required when using `content_base64`. Optional with `source_url` or `file_path`, inferred from the URL path, `Content-Disposition` header, or file path if omitted, but always prefer passing an explicit filename.
- `source_url` (string, conditional): HTTP(S) URL to download from.
- `content_base64` (string, conditional): File content as base64.
- `file_path` (string, conditional): Absolute path to a file on the server. Restricted to `ATTACHMENTS_DIR` and directories in `REDMINE_MCP_UPLOAD_FILE_ROOTS`.
- `description` (string, optional): Human-readable description.
- `version_id` (integer, optional): Version/release ID to attach the file to (use `list_redmine_versions` to discover valid IDs).

Returns: Dictionary containing the uploaded file's metadata, or `{"error": "..."}` on failure.

### `delete_file`

Parameters:
- `file_id` (integer, required): ID of the attachment to delete (from `list_files`).
- `confirm_delete_any_attachment` (boolean, optional): Bypass the project-scope check to delete issue/wiki/news attachments. Default: `false`.

Returns: `{"success": true, "deleted_file_id": <id>}` on success.

### `get_redmine_attachment`

Parameters:
- `attachment_id` (integer, required): The ID of the attachment to retrieve

Returns: `{uri, uri_type, filename, content_type, size, expires_at, attachment_id}` in HTTP mode (`uri_type="http"`), or `{file_path, uri_type, filename, content_type, size, expires_at, attachment_id}` in stdio mode (`uri_type="file"`).

### `cleanup_attachment_files`

Parameters: none

Returns: Cleanup statistics:

## Checklist Tools

### `get_checklist`

Parameters:
- `issue_id` (int, Yes): The ID of the issue whose checklist to retrieve

Returns: | Field | Type | Description |

### `update_checklist_item`

Parameters:
- `checklist_item_id` (int, Yes): The ID of the checklist item to update
- `subject` (string, No): New text for the checklist item
- `is_done` (bool, No): New done state
- `position` (int, No): New position/order

Returns: | Field | Type | Description |

### `create_checklist_item`

Parameters:
- `issue_id` (int, Yes): The ID of the issue to add the checklist item to
- `subject` (string, Yes): Text of the new checklist item or section header
- `is_section` (bool, No): When `true`, creates a section header rather than a checkable item
- `is_done` (bool, No): Initial done state for checkable items (ignored when `is_section=true`)
- `position` (int, No): 1-based position in the checklist. Omit to append at the end

Returns: | Field | Type | Description |

## Gantt Chart

### `get_gantt_chart`

Parameters:
- `project_id` (int or string, Yes): Project identifier
- `start_date_after` (string, No): `YYYY-MM-DD` filter (issues with `start_date >= this`)
- `due_date_before` (string, No): `YYYY-MM-DD` filter (issues with `due_date <= this`)
- `include_closed` (bool, No): Include closed issues. Default `false` keeps response size and pagination cost low on long-lived projects; set to `true` for full historical timelines.
- `limit` (int, No): Max issues (1–500)

Returns: | Field | Type | Description |

## Products (RedmineUP Products plugin)

### `manage_product`

Parameters:
- `action` (string, required): Allowed: `list`, `get`, `create`, `update`
- `project_id` (integer or string, optional): For `list`, filters products by project (omitted = all accessible). For `create`, optionally associates the new product with a project
- `limit` (integer, optional): For `list`, max results per call (default `100`). Redmine caps `limit` at 100 server-side; values above are clamped
- `product_id` (integer): Required for `get` and `update`
- `name` (string): Required for `create`
- `status_id` (integer, optional): For `create`. Must be `1` (Active, default) or `2` (Inactive)
- `description`, `code` (string, optional): For `create`
- `price` (float, optional): For `create`
- `currency` (string, optional): For `create` (e.g., `"USD"`)
- `category_id` (integer, optional): For `create`
- `tag_list` (string, optional): For `create`, comma-separated tags
- `custom_fields` (list, optional): For `create`, list of `{"id": N, "value": ...}` dicts
- `fields` (dict): For `update`, fields to update. Allowed keys: `name`, `description`, `price`, `currency`, `status_id`, `code`, `project_id`, `category_id`, `tag_list`, `custom_fields`. Unknown keys are silently filtered

Returns: - `list`: array of product dicts

## Contacts / CRM (RedmineUP CRM plugin)

### `manage_contact`

Parameters:
- `action` (string, required): Allowed: `list`, `get`, `create`, `update`, `delete`, `assign_to_project`, `remove_from_project`
- `project_id` (integer or string): For `list`, optional project filter. For `create`, required (project to associate the new contact with). For `assign_to_project` / `remove_from_project`, the project to attach to or detach from
- `search` (string, optional): For `list`, free-text search (matches name/company/email)
- `tags` (string, optional): For `list`, comma-separated tag filter
- `assigned_to_id` (integer, optional): For `list`, filter by assignee user ID
- `limit` (integer, optional): For `list`, max results per call (default `100`, capped at 100 by Redmine)
- `contact_id` (integer): Required for all actions except `list` and `create`
- `include` (string, optional): For `get`, comma-separated includes (`notes`, `deals`, `contacts`)
- `first_name` (string): Required for `create`
- `last_name`, `company`, `email`, `phone` (string, optional): For `create`
- `is_company` (boolean, optional): For `create`. `true` to mark as a company entity (default `false`)
- `visibility` (integer, optional): For `create`. `0`=Project (default), `1`=Public, `2`=Private
- `fields` (dict): For `update`, fields to update. Allowed keys: `first_name`, `last_name`, `middle_name`, `company`, `job_title`, `phone`, `email`, `website`, `skype_name`, `birthday`, `background`, `address_attributes`, `tag_list`, `is_company`, `assigned_to_id`, `custom_fields`, `visibility`, `project_id`. For `create`, additional fields beyond the named parameters

Returns: - `list`: array of contact dicts

## Documents (DMSF plugin)

### `manage_document`

Parameters:
- `list` (, `project_id`):
- `get` (, `document_id`):
- `create` (, `project_id`, `filename`, `content_base64`):
- `update` (, `document_id`, `fields`):

Returns: - `list`: list of node dicts. Each node has `id`, `type` (`file` / `folder` / `file-link` / `folder-link`), `filename`, `title`, `name`, `description`, `version`, `size`, `content_type`, `folder_id`, `project_id`, `author` (`{id, name}`), `created_on`, `updated_on`.

## Meta

### `get_mcp_server_info`

Parameters: none

Returns: - `server_version` (string): the deployed package version (from `importlib.metadata`). The literal `"0.0.0+unknown"` when the package metadata is unavailable (rare; source-tree runs without an editable install).

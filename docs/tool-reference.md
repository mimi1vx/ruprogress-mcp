# Tool reference

Generated from the live router by `cargo test -p ruprogress-mcp docs_reference` (`tests/tool_reference_doc.rs`) — do not hand-edit. Run with `UPDATE_DOCS=1` to regenerate after a tool's schema changes.

Every tool this build can register is listed, including the ones behind a plugin or admin flag; a tool without a "Gated by" line is registered unconditionally. "Kind" is `write` for tools read-only mode always hides, `partial` for tools with a mix of read/write `action`s, and `read` otherwise. "Required scopes" is this server's `oauth`/`oauth-proxy` scope-enforcement requirement for the common case; see `docs/tool-contract.md` for the argument-sensitive exceptions.

## Meta

### `get_mcp_server_info`

Return the MCP server's version, read-only/auth mode, plugin flags, and the identity of the authenticated Redmine user (or null if Redmine is unreachable). Use this once at the start of a session to learn what the server can do before calling other tools. Plugin-gated tools (e.g. get_checklist) are absent from tools/list unless their plugin_flags entry is on.

- **Kind:** read
- **Required scopes:** any authenticated token

**Parameters**

*(no parameters)*

**Output:** object: `active_sessions`, `auth_mode`, `autofill_required_custom_fields`, `current_user`, `oauth_scope_enforcement`, `plugin_flags`, `read_only_mode`, `registered_clients`, `required_custom_field_defaults_count`, `server_version`, `transport`

## Discovery

### `list_redmine_trackers`

List every tracker (Bug, Feature, ...) configured on the Redmine instance. Use this to resolve a tracker name to an id before creating an issue, when no project is known yet. Prefer list_project_trackers when a project id is available, since a project can restrict which trackers it accepts. An empty list means no trackers are configured — do not retry with the same arguments.

- **Kind:** read
- **Required scopes:** any authenticated token

**Parameters**

*(no parameters)*

**Output:** object: `trackers`

### `list_project_trackers`

List the trackers enabled for a specific project (numeric id or slug identifier). Use this instead of list_redmine_trackers whenever a project is known, since a project's settings can restrict which trackers it accepts. An empty list means no trackers are enabled for this project — do not retry with the same arguments.

- **Kind:** read
- **Required scopes:** `view_project`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `project_id` | integer \| string | yes | The project to list enabled trackers for: numeric id or slug |

**Output:** object: `trackers`

### `list_redmine_issue_statuses`

List every issue status (New, In Progress, Closed, ...) configured on the Redmine instance, including which ones count as closed. Use this to resolve a status name to an id before filtering or updating issues. An empty list would mean the instance has none configured — do not retry with the same arguments.

- **Kind:** read
- **Required scopes:** any authenticated token

**Parameters**

*(no parameters)*

**Output:** object: `issue_statuses`

### `list_redmine_issue_priorities`

List every issue priority (Low, Normal, High, ...) configured on the Redmine instance. Use this to resolve a priority name to an id before creating or updating an issue. An empty list means no priorities are configured — do not retry with the same arguments.

- **Kind:** read
- **Required scopes:** any authenticated token

**Parameters**

*(no parameters)*

**Output:** object: `issue_priorities`

### `list_redmine_users`

List Redmine user accounts, optionally filtered by name or group. Requires an admin credential. Use this to resolve a user's name to an id before assigning an issue. If this returns a FORBIDDEN error, the credential is not an admin — do not retry; call get_current_user to check your own identity, or ask the user for an admin account.

- **Kind:** read
- **Required scopes:** any authenticated token

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `group_id` | integer \| null | no | Restrict to members of this group. |
| `limit` | integer \| null | no | Page size, clamped to 1-100. Defaults to 25. |
| `name` | string \| null | no | Filter by name: matches login, firstname, lastname, or a |
| `offset` | integer \| null | no | Offset of the first result. Defaults to 0. |

**Output:** object: `pagination`, `users`

### `get_current_user`

Retrieve the currently authenticated user's profile (id, login, name, mail, admin flag). Use this to resolve "me" or to check whether the credential is an admin before calling admin-only tools like list_redmine_users.

- **Kind:** read
- **Required scopes:** any authenticated token

**Parameters**

*(no parameters)*

**Output:** object: `admin`, `created_on`, `firstname`, `id`, `last_login_on`, `lastname`, `login`, `mail`

### `list_redmine_queries`

List the current user's saved (custom) issue queries. Redmine has no API to create, update, or delete saved queries, so this is the only query-related tool — do not look for a manage_redmine_query tool. Use this to resolve a saved query's name to an id. An empty list means the user has no saved queries.

- **Kind:** read
- **Required scopes:** `view_issues`

**Parameters**

*(no parameters)*

**Output:** object: `pagination`, `queries`

## Projects

### `list_redmine_projects`

List all accessible projects in the Redmine instance. Use this first to resolve a project's numeric id or identifier before calling project- or issue-scoped tools. An empty list means the credential cannot see any projects — do not retry with the same arguments.

- **Kind:** read
- **Required scopes:** `view_project`

**Parameters**

*(no parameters)*

**Output:** object: `pagination`, `projects`

### `list_project_issue_custom_fields`

List issue custom fields configured for a project, including allowed values and tracker bindings. Use this before create_redmine_issue/update_redmine_issue to discover which fields a project accepts. Requires an admin credential (this endpoint is admin-only regardless of field sensitivity). is_required is not authoritative: workflow rules can still require a field reported as optional.

- **Kind:** read
- **Required scopes:** any authenticated token

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `project_id` | integer \| string | yes | The project to list issue custom fields for: numeric id or slug |
| `tracker_id` | integer \| null | no | Restrict output to fields applicable to this tracker id. |

**Output:** object: `custom_fields`

### `summarize_project_status`

Summarize project status: recent issue activity, status/priority/assignee breakdowns, and open/closed totals over a configurable time window. Use this when the user wants a written project health summary, not a raw issue list. The breakdowns are computed over a capped recent-issue sample (see sample_truncated), not necessarily every issue.

- **Kind:** read
- **Required scopes:** `view_project`, `view_issues`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `days` | integer \| null | no | Number of days of history to analyze for the recent-activity |
| `project_id` | integer | yes | The project to summarize. |

**Output:** object: `analysis_period_days`, `assignee_breakdown`, `priority_breakdown`, `project_id`, `project_name`, `recent_activity`, `sample_size`, `sample_truncated`, `status_breakdown`, `totals`

### `list_redmine_versions`

List versions (roadmap milestones) for a Redmine project. Use this to discover a target version's id before filtering issues by fixed_version_id or calling manage_redmine_version. An empty list means the project has no versions configured — do not retry with the same arguments.

- **Kind:** read
- **Required scopes:** `view_issues`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `project_id` | integer \| string | yes | The project to list versions for: numeric id or slug identifier. |
| `status_filter` | string \| null | no | Filter by version status. Applied after fetching every version — |

**Output:** object: `versions`

### `manage_redmine_version`

Create, update, or delete a Redmine version (roadmap milestone). Use this when the user wants to add, change, or remove a milestone. action="create" needs project_id and name; "update"/"delete" need version_id (find one via list_redmine_versions). Blocked entirely in read-only mode.

- **Kind:** write
- **Required scopes:** `manage_versions`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `action` | string | yes | Operation to perform. |
| `description` | string \| null | no |  |
| `due_date` | string \| null | no | `YYYY-MM-DD`. |
| `name` | string \| null | no | Version name. Required for `action = "create"`, optional for |
| `project_id` | integer \| string \| null | no | Project id or identifier. Required for `action = "create"`. |
| `sharing` | string \| null | no | Defaults to `none` on create if omitted. |
| `status` | string \| null | no | Defaults to `open` on create if omitted. |
| `version_id` | integer \| null | no | Version id. Required for `action = "update"` and `action = "delete"`. |
| `wiki_page_title` | string \| null | no |  |

**Output:** object: `deleted_version_id`, `success`, `version`

### `list_project_members`

List all members (users and groups) of a Redmine project along with their assigned roles. Use this to see who has access to a project, or before manage_project_member to find a membership_id to update or remove.

- **Kind:** read
- **Required scopes:** `view_members`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `project_id` | integer \| string | yes | Numeric id or slug identifier. |

**Output:** object: `memberships`, `pagination`

### `list_redmine_roles`

List all roles defined in the Redmine instance (id and name only). Call this before manage_project_member(action="add"|"update") to discover valid role_ids — role ids vary between Redmine instances and must not be guessed. Unlike list_redmine_users, this does not require an admin credential.

- **Kind:** read
- **Required scopes:** any authenticated token

**Parameters**

*(no parameters)*

**Output:** object: `roles`

### `get_project_modules`

Retrieve the list of enabled modules for a project (e.g. issue_tracking, time_tracking, wiki, repository). Use this to check whether a feature (like time tracking or the wiki) is available in a project before calling a module-specific tool.

- **Kind:** read
- **Required scopes:** `view_project`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `project_id` | integer \| string | yes | Numeric id or slug identifier. |

**Output:** object: `enabled_modules`, `project_id`, `project_name`

### `manage_project_member`

Add, update, or remove a Redmine project membership. Use this to grant or change project access. action="add" needs project_id, one of user_id/group_id, and role_ids; "update" needs membership_id and role_ids; "remove" needs membership_id (use list_redmine_roles first to find valid role_ids). Blocked in read-only mode; inherited memberships must be removed from the parent project instead.

- **Kind:** write
- **Required scopes:** `manage_members`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `action` | string | yes |  |
| `group_id` | integer \| null | no | Exactly one of `user_id`/`group_id` is required for `action = "add"`. |
| `membership_id` | integer \| null | no | Required for `action = "update"` and `action = "remove"`. |
| `project_id` | integer \| string \| null | no | Required for `action = "add"`. |
| `role_ids` | array<integer> | no | Non-empty. Required for `action = "add"` and `action = "update"`. Use |
| `user_id` | integer \| null | no | Exactly one of `user_id`/`group_id` is required for `action = "add"`. |

**Output:** object: `deleted_membership_id`, `membership`, `success`

## Issues

### `get_redmine_issue`

Retrieve full details of one Redmine issue by numeric id, including by default journals and attachments. Use this when the issue id is already known; use list_redmine_issues or search_redmine_issues to find one first. include_watchers/include_relations/include_children default false; journal_limit pages long journal history. children nests one level deep — use list_subtasks for deeper trees.

- **Kind:** read
- **Required scopes:** `view_issues`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `include_attachments` | boolean | no | Include attachment metadata. Default true. |
| `include_children` | boolean | no | Include direct sub-issues, nested one level deep. Default false. Use |
| `include_custom_fields` | boolean | no | Include custom field values. Default true. |
| `include_journals` | boolean | no | Include journals (comments and field-change history). Default true. |
| `include_relations` | boolean | no | Include issue relations. Default false. |
| `include_watchers` | boolean | no | Include the watcher list. Default false. |
| `issue_id` | integer | yes | The id of the issue to retrieve. |
| `journal_limit` | integer \| null | no | Maximum number of journals to return, applied client-side after |
| `journal_offset` | integer \| null | no | Number of journals to skip, used with `journal_limit`. Default 0. |

**Output:** object: `agile_position`, `agile_sprint_id`, `assigned_to`, `attachments`, `author`, `category`, `children`, `closed_on`, `created_on`, `custom_fields`, `description`, `done_ratio`, `due_date`, `estimated_hours`, `fixed_version`, `id`, `is_private`, `journal_pagination`, `journals`, `parent`, `priority`, `project`, `relations`, `spent_hours`, `start_date`, `status`, `story_points`, `subject`, `tags`, `tracker`, `updated_on`, `watchers`

### `list_redmine_issues`

List Redmine issues with flexible filtering and pagination. Supports filtering by project, status, tracker, priority, assignee, and target version. Use this for advanced filtering by field value; use search_redmine_issues for free-text search instead. An empty list means nothing matched — try widening the filters before retrying.

- **Kind:** read
- **Required scopes:** `view_issues`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `assigned_to_id` | integer \| string \| null | no | Filter by assignee: a numeric user id, or `"me"` for the credential's |
| `fields` | array<string> | no | Restrict which fields each issue carries. Omit for every field; |
| `fixed_version_id` | integer \| null | no | Filter by target version (roadmap milestone) id. |
| `include_pagination_info` | boolean | no | Include the `pagination` member in the result. Default false. |
| `limit` | integer \| null | no | Page size, clamped to 1-1000. Defaults to 25. |
| `offset` | integer \| null | no | Offset of the first result. Defaults to 0. |
| `priority_id` | integer \| null | no | Filter by priority id. |
| `project_id` | integer \| string \| null | no | Restrict to one project: numeric id or slug identifier. |
| `sort` | string \| null | no | Redmine sort syntax, e.g. `"updated_on:desc"`. |
| `status_id` | integer \| null | no | Filter by status id. Absent means Redmine's own default (open |
| `tracker_id` | integer \| null | no | Filter by tracker id. |

**Output:** object: `issues`, `pagination`

### `search_redmine_issues`

Search issues by free text, with pagination and native Search API filters (scope, open_issues). Use this for text-based search; use list_redmine_issues for filtering by exact field values (project_id, status_id, priority_id, etc). An empty list means nothing matched the search text.

- **Kind:** read
- **Required scopes:** `search_project`, `view_issues`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `fields` | array<string> | no | Restrict which fields each issue carries. See `list_redmine_issues`. |
| `include_pagination_info` | boolean | no | Include the `pagination` member in the result. Default false. |
| `limit` | integer \| null | no | Page size, clamped to 1-1000. Defaults to 25. |
| `offset` | integer \| null | no | Offset of the first result. Defaults to 0. |
| `open_issues` | boolean | no | Search only open issues. Default false. |
| `query` | string | yes | The search text. Must not be empty. |
| `scope` | string \| null | no | Restrict which projects are searched. Default: all. |

**Output:** object: `issues`, `pagination`

### `list_subtasks`

List subtasks (child issues) of a given issue, including closed ones. Use this to see the full immediate-child list; get_redmine_issue's own children field nests only one level. An empty list means the issue has no subtasks.

- **Kind:** read
- **Required scopes:** `view_issues`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `issue_id` | integer | yes | The issue id. |

**Output:** object: `subtasks`

### `get_private_notes`

Retrieve only the private notes (journals with private_notes=true and non-empty text) of an issue. Use this instead of get_redmine_issue when only private notes are wanted. An empty list means either no private notes exist, or the credential lacks the "View private notes" permission — this tool cannot tell the two apart, so do not assume an empty result means none exist.

- **Kind:** read
- **Required scopes:** `view_issues`, `view_private_notes`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `issue_id` | integer | yes | The issue id. |

**Output:** object: `private_notes`

### `create_redmine_issue`

Create a new Redmine issue. Use this to add a task, bug, or feature request to a project. Only project_id and subject are required; every other field defaults to the project's/tracker's own default when omitted. A rejected required custom field may be autofilled once and reported in autofilled_custom_fields, if the operator enabled that. Write tool; blocked in read-only mode.

- **Kind:** write
- **Required scopes:** `add_issues`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `assigned_to_id` | integer \| null | no | Who to assign the issue to. |
| `category_id` | integer \| null | no | The issue category id. |
| `custom_fields` | array<object> | no | Custom field values to set on the new issue. Each entry gives |
| `description` | string \| null | no | The issue description. |
| `done_ratio` | integer \| null | no | Percent done, 0-100. |
| `due_date` | string \| null | no | Planned due date (`YYYY-MM-DD`). |
| `estimated_hours` | number \| null | no | Estimated hours. |
| `fixed_version_id` | integer \| null | no | The target version (roadmap milestone) id. |
| `is_private` | boolean \| null | no | Whether the issue is private. |
| `parent_issue_id` | integer \| null | no | Parent issue id, to create this as a sub-issue. |
| `priority_id` | integer \| null | no | The priority id, if not the default. |
| `project_id` | integer \| string | yes | The project to create the issue in: numeric id or slug identifier. |
| `start_date` | string \| null | no | Planned start date (`YYYY-MM-DD`). |
| `status_id` | integer \| null | no | The status id, if not the tracker's default. |
| `subject` | string | yes | The issue subject/title. Must not be empty. |
| `tag_list` | array<string> | no | Tags to set on the new issue (`AlphaNodes` `additional_tags` plugin). |
| `tracker_id` | integer \| null | no | The tracker id, if not the project's default. |
| `uploads` | array<object> | no | Files to attach to the issue in this same request. Maximum 10 items; |

**Output:** object: `autofilled_custom_fields`, `issue`, `success`

### `update_redmine_issue`

Update fields on an existing issue, or add a note to its history. Use this when a field needs to change or a comment should be added; omit any parameter to leave that field unchanged. If the operator enabled required-custom-field autofill, a rejected required custom field may be filled from its default and reported in autofilled_custom_fields, at most once. Write tool; blocked in read-only mode.

- **Kind:** write
- **Required scopes:** `edit_issues`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `agile_position` | integer \| null | no | New position within its sprint/board (`RedmineUP` Agile plugin). |
| `agile_sprint_id` | integer \| null | no | New sprint id (`RedmineUP` Agile plugin). `0` removes the issue from |
| `assigned_to_id` | integer \| null | no | New assignee user id. |
| `category_id` | integer \| null | no | New category id. |
| `custom_fields` | array<object> | no | Custom field values to set, if changing any. Each entry gives |
| `description` | string \| null | no | New description. An empty string clears it; omit to leave unchanged. |
| `done_ratio` | integer \| null | no | New percent done, 0-100. |
| `due_date` | string \| null | no | New planned due date. |
| `estimated_hours` | number \| null | no | New estimated hours. |
| `fixed_version_id` | integer \| null | no | New target version id. |
| `is_private` | boolean \| null | no | New privacy flag. |
| `issue_id` | integer | yes | The id of the issue to update. |
| `notes` | string \| null | no | A note to add to the issue's history, independent of any field |
| `parent_issue_id` | integer \| null | no | New parent issue id, to reparent this issue. |
| `priority_id` | integer \| null | no | New priority id. |
| `private_notes` | boolean \| null | no | Whether the note added via `notes` is private. Requires the "set |
| `start_date` | string \| null | no | New planned start date. |
| `status_id` | integer \| null | no | New status id. |
| `story_points` | integer \| null | no | New story points (`RedmineUP` Agile plugin). Omit to leave unchanged, |
| `subject` | string \| null | no | New subject, if changing it. |
| `tag_list` | array<string> | no | Replaces the issue's full tag set (`AlphaNodes` `additional_tags` |
| `tracker_id` | integer \| null | no | New tracker id. |
| `uploads` | array<object> | no | Files to attach to the issue in this same request. Maximum 10 items; |

**Output:** object: `autofilled_custom_fields`, `issue`, `success`

### `delete_redmine_issue`

Delete a Redmine issue. Refuses by default and returns an impact preview (children/journals/attachments/relations/time-entry counts); pass confirm_delete=true to proceed, and confirm_delete_with_children=true too if it has subtasks. A refusal is a normal result, not an error. Use when the user explicitly asks to delete an issue. Write tool; blocked in read-only mode.

- **Kind:** write
- **Required scopes:** `delete_issues`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `confirm_delete` | boolean | no | When `false` (default), the tool refuses and returns an impact |
| `confirm_delete_with_children` | boolean | no | When the issue has direct subtasks, `confirm_delete=true` alone still |
| `issue_id` | integer | yes | The id of the issue to delete. |

**Output:** object: `cascade_deleted`, `code`, `deleted_issue_id`, `error`, `hint`, `impact`, `success`

### `copy_issue`

Copy an issue to a new one, optionally into another project, optionally recursively copying subtasks. Most fields are copied from the source unless overridden; status is never copied. Attachments are never copied. Bounded to 50 issues per call. Use this instead of create_redmine_issue when duplicating an existing issue. Write tool; blocked in read-only mode.

- **Kind:** write
- **Required scopes:** `view_issues`, `add_issues`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `assigned_to_id` | integer \| null | no | Override the assignee on the copy. Defaults to the source's. |
| `category_id` | integer \| null | no | Override the category on the copy. Defaults to the source's. |
| `copy_subtasks` | boolean | no | Recursively copy the source's direct subtasks (and their own |
| `description` | string \| null | no | Override the description on the copy. Defaults to the source's. |
| `fixed_version_id` | integer \| null | no | Override the target version on the copy. Defaults to the source's. |
| `issue_id` | integer | yes | The id of the source issue to copy. |
| `link_original` | boolean | no | Create a `copied_to`/`copied_from` relation between the original and |
| `priority_id` | integer \| null | no | Override the priority on the copy. Defaults to the source's. |
| `project_id` | integer \| string \| null | no | Target project for the copy: numeric id or slug identifier. Defaults |
| `subject` | string \| null | no | New subject for the copy. Defaults to the source subject. |
| `tracker_id` | integer \| null | no | Override the tracker on the copy. Defaults to the source's. |

**Output:** object: `issue`, `subtasks_copied`, `subtasks_truncated`, `success`

### `manage_issue_relation`

Manage relations between issues (relates, blocks, precedes, ...). Use this to list (issue_id, works read-only), create (issue_id+issue_to_id), or delete (relation_id) a relation. create/delete are blocked in read-only mode.

- **Kind:** partial (per `action`)
- **Required scopes:** `list`: `view_issues`; `create`: `manage_issue_relations`; `delete`: `manage_issue_relations`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `action` | string | yes | Operation to perform. `list` is always available, even in read-only |
| `delay` | integer \| null | no | Delay in days. Only meaningful for `precedes`. |
| `issue_id` | integer \| null | no | Source issue id. Required for `action="list"` and `action="create"`. |
| `issue_to_id` | integer \| null | no | Target issue id. Required for `action="create"`. |
| `relation_id` | integer \| null | no | Relation id. Required for `action="delete"`. |
| `relation_type` | string \| null | no | One of `relates`, `duplicates`, `duplicated`, `blocks`, `blocked`, |

**Output:** object: `deleted_relation_id`, `relation`, `relations`, `success`

### `manage_issue_watcher`

Add or remove a watcher on an issue. Use this to subscribe/unsubscribe a user to an issue's notifications. Write tool; blocked in read-only mode.

- **Kind:** write
- **Required scopes:** `add`: `add_issue_watchers`; `remove`: `delete_issue_watchers`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `action` | string | yes |  |
| `issue_id` | integer | yes | The issue id. |
| `user_id` | integer | yes | The user id to add or remove as a watcher. |

**Output:** object: `issue_id`, `success`, `user_id`

### `manage_issue_note`

Edit an issue note's text and/or private flag. Use this to edit (journal_id+notes; empty string clears it) or set_private (journal_id+is_private) alone. journal_id comes from get_redmine_issue or get_private_notes. Write tool; blocked in read-only mode.

- **Kind:** write
- **Required scopes:** `edit`: `edit_issue_notes`; `set_private`: `set_notes_private`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `action` | string | yes |  |
| `is_private` | boolean \| null | no | `true` to mark the note private, `false` to make it public. Required |
| `journal_id` | integer | yes | The journal (note) id, from `get_redmine_issue` with |
| `notes` | string \| null | no | New note text. May be empty to clear it. Required for |
| `private_notes` | boolean \| null | no | Toggle the private flag while editing. Optional for |

**Output:** object: `journal_id`, `notes`, `private_notes`, `success`

### `manage_issue_category`

Manage issue categories on a project. Use this to list (project_id, works read-only), create (project_id+name), update, or delete (category_id) categories; delete accepts reassign_to_id to move issues instead of leaving them uncategorised. create/update/delete are blocked in read-only mode.

- **Kind:** partial (per `action`)
- **Required scopes:** `list`: `view_issues`; `create`: `manage_categories`; `update`: `manage_categories`; `delete`: `manage_categories`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `action` | string | yes | Operation to perform. `list` is always available, even in read-only |
| `assigned_to_id` | integer \| null | no | Default assignee user id. For `create`/`update`. |
| `category_id` | integer \| null | no | Category id. Required for `action="update"` and `action="delete"`. |
| `name` | string \| null | no | Category name. Required for `action="create"`; optional (but not |
| `project_id` | integer \| string \| null | no | Project identifier. Required for `action="list"` and |
| `reassign_to_id` | integer \| null | no | Reassign the deleted category's issues to this category id instead |

**Output:** object: `categories`, `category`, `deleted_category_id`, `success`

## Time tracking

### `list_time_entries`

List logged time entries, optionally filtered by project, issue, user, or date range. Use this to review time already logged before summarizing or exporting it. from_date/to_date are translated into Redmine's own spent_on filter syntax. An empty list means no matching entries — do not retry with the same arguments.

- **Kind:** read
- **Required scopes:** `view_time_entries`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `from_date` | string \| null | no | Only entries spent on or after this date (`YYYY-MM-DD`). |
| `issue_id` | integer \| null | no | Restrict to one issue. |
| `limit` | integer \| null | no | Page size, clamped to 1-100. Defaults to 25. |
| `offset` | integer \| null | no | Offset of the first result. Defaults to 0. |
| `project_id` | integer \| string \| null | no | Restrict to one project: numeric id or slug identifier. |
| `to_date` | string \| null | no | Only entries spent on or before this date (`YYYY-MM-DD`). |
| `user_id` | integer \| string \| null | no | Restrict to one user: a numeric user id, or `"me"` for the |

**Output:** object: `pagination`, `time_entries`

### `manage_time_entry`

Log or update a time entry against an issue or project. Use this when the user wants to record time spent. action="create" needs hours and at least one of project_id/issue_id; "update" needs time_entry_id. Blocked entirely in read-only mode.

- **Kind:** write
- **Required scopes:** `create`: `log_time`; `update`: `edit_time_entries`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `action` | string | yes | Operation to perform. |
| `activity_id` | integer \| null | no |  |
| `comments` | string \| null | no | Description. An empty string clears the field on `action = "update"`; |
| `hours` | number \| null | no | Hours spent. Required (and must be positive) for `action = "create"`; |
| `issue_id` | integer \| null | no | Required for `action = "create"` if `project_id` is not provided. |
| `project_id` | integer \| string \| null | no | Project id or identifier. Required for `action = "create"` if |
| `spent_on` | string \| null | no | `YYYY-MM-DD`. Defaults to today on create if omitted. |
| `time_entry_id` | integer \| null | no | Entry id to update. Required for `action = "update"`. |
| `user_id` | integer \| null | no | Log time on behalf of this user (`action = "create"` only). Requires |

**Output:** object: `success`, `time_entry`

### `list_time_entry_activities`

List time-tracking activities (Development, QA, ...). Pass project_id to see only activities enabled for that project (Redmine 3.4.0+); without it, lists every activity defined on the instance, including active/is_default flags the project-scoped form does not carry. Use this to resolve an activity name to an id before manage_time_entry/import_time_entries.

- **Kind:** read
- **Required scopes:** any authenticated token

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `project_id` | integer \| string \| null | no | When given, returns this project's enabled activities instead of the |

**Output:** object: `time_entry_activities`

### `import_time_entries`

Bulk-log up to 500 time entries in one call. Use this instead of repeated manage_time_entry calls when importing time from an external source. Each entry needs hours and at least one of project_id/issue_id. Continues past a failing entry by default and reports every outcome; stop_on_error=true halts at the first failure. Created entries are never rolled back. Blocked in read-only mode.

- **Kind:** write
- **Required scopes:** `log_time`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `entries` | array<object> | yes | At most 500 entries per call. |
| `stop_on_error` | boolean | no | Abort on the first error. Default `false`: keep attempting every |

**Output:** object: `attempted`, `failed`, `results`, `succeeded`, `total`

## Search & wiki

### `search_entire_redmine`

Search across issues and wiki pages in one call. Use this for a broad text search when the resource type is not yet known. Prefer search_redmine_issues for issue-only search with richer filtering (scope, open_issues, field selection). Results are thin (id, title, excerpt only) — follow up with get_redmine_issue or manage_redmine_wiki_page(action="get") for full details.

- **Kind:** read
- **Required scopes:** `search_project`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `limit` | integer \| null | no | Maximum results to return, clamped to 1-100. Defaults to 100. |
| `offset` | integer \| null | no | Pagination offset. Defaults to 0. |
| `query` | string | yes | Text to search for. |
| `resources` | array<string> | no | Restrict to these resource types. Defaults to both issues and wiki |

**Output:** object: `pagination`, `results`, `results_by_type`

### `manage_redmine_wiki_page`

List, get, create, update, delete, or rename a wiki page. Use this to manage project documentation. project_id and action are required; wiki_page_title is required except for list. create/update need text; rename needs new_title. list/get work in read-only mode; the rest are blocked. Deleting a page un-parents its children rather than deleting them.

- **Kind:** partial (per `action`)
- **Required scopes:** `list`: `view_wiki_pages`; `get`: `view_wiki_pages`; `create`: `edit_wiki_pages`; `update`: `edit_wiki_pages`; `delete`: `delete_wiki_pages`; `rename`: `rename_wiki_pages`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `action` | string | yes | Operation to perform. |
| `comments` | string \| null | no | Change-log comment for `create`/`update`/`rename`. |
| `include_attachments` | boolean \| null | no | Include attachment metadata in the `get` response. Defaults to |
| `new_title` | string \| null | no | The new title. Required for `rename`; must differ from |
| `project_id` | integer \| string | yes | The project the wiki page belongs to. Required for every action. |
| `redirect_existing_links` | boolean \| null | no | When `true` (default), `rename` leaves a redirect from the old title |
| `text` | string \| null | no | Page content. Required for `create` and `update`. |
| `version` | integer \| null | no | Specific revision to fetch. `get` only; defaults to the latest. |
| `wiki_page_title` | string \| null | no | The page title. Required for every action except `list`. |

**Output:** object: `deleted_title`, `message`, `page`, `pages`, `success`

## Gantt

### `get_gantt_chart`

Build a Gantt-chart projection for a project: issues with start/due dates, percent-done, and parent hierarchy, plus the project's versions as milestones. Use this when the user wants a timeline or roadmap view rather than a flat issue list. Defaults to open issues only; set include_closed=true for a full historical timeline.

- **Kind:** read
- **Required scopes:** `view_issues`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `due_date_before` | string \| null | no | Only include issues with `due_date <= this` (`YYYY-MM-DD`). |
| `include_closed` | boolean \| null | no | Include closed issues. Defaults to `false` to keep response size and |
| `limit` | integer \| null | no | Max issues to return, clamped to 1-500. Defaults to 100. |
| `project_id` | integer \| string | yes | The project to chart: numeric id or slug identifier. |
| `start_date_after` | string \| null | no | Only include issues with `start_date >= this` (`YYYY-MM-DD`). |

**Output:** object: `issues`, `milestones`, `pagination`, `project_id`, `project_name`

## Files

### `get_redmine_attachment`

Download a Redmine attachment by numeric id, staging it in the server local file store. Use this when the actual file bytes are needed, not just metadata. Returns a /files/{uuid} URL (HTTP transport) or an absolute file_path (stdio) per uri_type; the copy expires after ATTACHMENT_EXPIRES_MINUTES (60 min default), so fetch promptly. attachment_id comes from get_redmine_issue or list_files.

- **Kind:** read
- **Required scopes:** `view_files`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `attachment_id` | integer | yes | The id of the attachment to retrieve. |

**Output:** object: `attachment_id`, `content_type`, `expires_at`, `file_path`, `filename`, `size`, `uri`, `uri_type`

### `list_files`

List files in a project's Files module (GET /projects/{id}/files.json) — not issue attachments (use get_redmine_issue for those) and not the DMSF plugin. Returns metadata only; call get_redmine_attachment with the returned id to download the actual bytes. Use this when the user asks what files are attached to a project.

- **Kind:** read
- **Required scopes:** `view_files`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `project_id` | integer \| string | yes | The project to list Files-module entries for: numeric id or slug |

**Output:** object: `files`

### `delete_file`

Delete a Redmine attachment by id. This can delete ANY attachment this credential can reach, not just project Files — issue and wiki attachments too — since Redmine does not report which container an attachment belongs to. Requires confirm_delete_any_attachment=true. Use this when the user explicitly asks to delete an attachment. Write tool; blocked in read-only mode.

- **Kind:** write
- **Required scopes:** `manage_files`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `confirm_delete_any_attachment` | boolean | no | `DELETE /attachments/{id}.json` deletes *any* attachment this |
| `file_id` | integer | yes | The id of the attachment to delete, from `list_files` or an issue's |

**Output:** object: `deleted_file_id`, `success`

### `upload_file`

Upload a file and attach it to a project's Files module. Exactly one of content_base64 (requires filename) or file_path is required; source_url is not supported and returns UNSUPPORTED_SOURCE. Both sources are capped at 50 MiB; file_path must additionally be inside ATTACHMENTS_DIR or REDMINE_MCP_UPLOAD_FILE_ROOTS. Use this when attaching a file to a project. Write tool; blocked in read-only mode.

- **Kind:** write
- **Required scopes:** `manage_files`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `content_base64` | string \| null | no | Raw file bytes, base64-encoded. Exactly one of `content_base64`/ |
| `description` | string \| null | no | Human-readable description shown in the Files module. |
| `file_path` | string \| null | no | Absolute path to a file already on this server: inside |
| `filename` | string \| null | no | Name the file should have in Redmine. Required when using |
| `project_id` | integer \| string | yes | The project to attach the uploaded file to. |
| `source_url` | string \| null | no | Not supported by this server. Present only so a caller who sends it |
| `version_id` | integer \| null | no | Attach to this version instead of the project directly. |

**Output:** object: `author`, `content_type`, `content_url`, `created_on`, `description`, `digest`, `downloads`, `filename`, `filesize`, `id`, `version`

### `cleanup_attachment_files`

Immediately sweep expired files out of the local attachment store, the same cleanup the background sweeper performs on a timer, and report how much was reclaimed. Local-disk-only; never touches Redmine, so it still works in read-only mode. Use this to free disk space now instead of waiting for CLEANUP_INTERVAL_MINUTES. Admin tool, requires REDMINE_MCP_EXPOSE_ADMIN_TOOLS=true.

- **Kind:** read
- **Gated by:** `REDMINE_MCP_EXPOSE_ADMIN_TOOLS`
- **Required scopes:** any authenticated token

**Parameters**

*(no parameters)*

**Output:** object: `cleaned_bytes`, `cleaned_files`, `cleaned_mb`

## Plugins: RedmineUP Checklists

### `get_checklist`

Get the checklist items on an issue (RedmineUP Checklists Pro plugin). Use this to see an issue's checklist before adding or editing an item; an empty list means the issue has no checklist — do not retry. A very large checklist may be silently truncated by this server's response-size caps, with no further page to fetch.

- **Kind:** read
- **Gated by:** `REDMINE_CHECKLISTS_ENABLED`
- **Required scopes:** any authenticated token

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `issue_id` | integer | yes | The issue whose checklist to retrieve. |

**Output:** object: `issue_id`, `items`, `total_count`

### `create_checklist_item`

Add a checklist item or section header to an issue (RedmineUP Checklists Pro plugin). Use this after get_checklist to add a new checkable item (is_section=false, the default) or a section header (is_section=true). Write tool; blocked in read-only mode.

- **Kind:** write
- **Gated by:** `REDMINE_CHECKLISTS_ENABLED`
- **Required scopes:** any authenticated token

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `is_done` | boolean \| null | no | Initial checked state for a checkable item. Sent as given even when |
| `is_section` | boolean \| null | no | `true` to create a section header rather than a checkable item. |
| `issue_id` | integer | yes | The issue to add the checklist item to. |
| `position` | integer \| null | no | 1-based position in the checklist. Omit to append at the end. |
| `subject` | string | yes | Text of the new checklist item, or the section header's title. Must |

**Output:** object: `checklist_item_id`, `is_done`, `is_section`, `issue_id`, `position`, `subject`, `success`

### `update_checklist_item`

Edit a checklist item's text, done state, or position (RedmineUP Checklists Pro plugin). Use this after get_checklist to change one existing item; at least one of subject/is_done/position is required. Write tool; blocked in read-only mode.

- **Kind:** write
- **Gated by:** `REDMINE_CHECKLISTS_ENABLED`
- **Required scopes:** any authenticated token

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `checklist_item_id` | integer | yes | The checklist item to update, from `get_checklist`. |
| `is_done` | boolean \| null | no | New checked state, if changing it. |
| `position` | integer \| null | no | New 1-based position, if changing it. |
| `subject` | string \| null | no | New text, if changing it. Must not be blank if given. |

**Output:** object: `checklist_item_id`, `success`, `updated_fields`

## Plugins: RedmineUP Products

### `manage_product`

List, get, create, or update RedmineUP products (RedmineUP Products plugin). There is no delete action. list/get work in read-only mode; create/update are blocked. name is required for create; product_id is required for get/update.

- **Kind:** partial (per `action`)
- **Gated by:** `REDMINE_PRODUCTS_ENABLED`
- **Required scopes:** any authenticated token

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `action` | string | yes | Operation to perform. There is no `delete` action — the Products |
| `category_id` | integer \| null | no | The product category id. For `create`/`update`. |
| `code` | string \| null | no | A short product code/SKU. For `create`/`update`. |
| `currency` | string \| null | no | The price's currency, e.g. `"USD"`. For `create`/`update`. |
| `custom_fields` | array<object> | no | Custom field values to set, by id. For `create`/`update`. |
| `description` | string \| null | no | Free-text description. For `create`/`update`. |
| `limit` | integer \| null | no | For `list`, max results per call, clamped to 1-100. Default 100. |
| `name` | string \| null | no | The product's display name. Required for `create`. |
| `offset` | integer \| null | no | For `list`, pagination offset. Default 0. |
| `price` | number \| null | no | Unit price. For `create`/`update`. |
| `product_id` | integer \| null | no | The product to act on. Required for `get` and `update`. |
| `project_id` | integer \| string \| null | no | For `list`, restrict to this project's products (omit for every |
| `status_id` | integer \| null | no | `1` = Active, `2` = Inactive. For `create`/`update`. Defaults to |
| `tag_list` | array<string> | no | Replaces the product's full tag set. For `create`/`update`. |

**Output:** object: `pagination`, `product`, `products`, `success`, `updated_fields`

## Plugins: RedmineUP CRM

### `manage_contact`

List, get, create, update, or delete a RedmineUP CRM contact, or attach/detach one from a project (RedmineUP CRM plugin). list/get work read-only; other actions are blocked. first_name required for create; contact_id required except for list/create; project_id required for create/assign_to_project/remove_from_project. assign_to_project does not create; remove_from_project does not delete.

- **Kind:** partial (per `action`)
- **Gated by:** `REDMINE_CRM_ENABLED`
- **Required scopes:** any authenticated token

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `action` | string | yes | Operation to perform. |
| `address` | object \| null | no |  |
| `assigned_to_id` | integer \| null | no | For `list`, filter by assignee user id. For `create`/`update`, the |
| `background` | string \| null | no |  |
| `birthday` | string \| null | no | `YYYY-MM-DD`. |
| `company` | string \| null | no |  |
| `contact_id` | integer \| null | no | The contact to act on. Required for every action except `list` and |
| `email` | string \| null | no |  |
| `first_name` | string \| null | no | Given name. Required for `create`. |
| `include` | array<string> | no | For `get`, additional data to include. |
| `is_company` | boolean \| null | no | `true` to mark this contact as a company rather than a person. |
| `job_title` | string \| null | no |  |
| `last_name` | string \| null | no |  |
| `limit` | integer \| null | no | For `list`, max results per call, clamped to 1-100. Default 100. |
| `middle_name` | string \| null | no |  |
| `offset` | integer \| null | no | For `list`, pagination offset. Default 0. |
| `phone` | string \| null | no |  |
| `project_id` | integer \| string \| null | no | For `list`, optional project filter. For `create`, required (the |
| `search` | string \| null | no | For `list`, free-text search (matches name/company/email). |
| `skype_name` | string \| null | no |  |
| `tags` | string \| null | no | For `list`, a comma-separated tag filter, passed through as given |
| `visibility` | integer \| null | no | `0` = Project (default), `1` = Public, `2` = Private. |
| `website` | string \| null | no |  |

**Output:** object: `contact`, `contacts`, `deleted_contact_id`, `message`, `pagination`, `success`, `updated_fields`

## Plugins: DMSF

### `manage_document`

List, get, create, or update documents in the DMSF plugin (redmine_dmsf, GPL v2; must be installed server-side, and its DMSF module replaces rather than complements Redmine's built-in Documents). There is no delete action. list/get work in read-only mode; create/update are blocked. create requires project_id and exactly one of content_base64 (requires name) or file_path, both capped at 50 MiB; its response is sparse ({document_id} only) — follow up with action="get". update always creates a new revision rather than replacing one, and requires document_id.

- **Kind:** partial (per `action`)
- **Gated by:** `REDMINE_DMSF_ENABLED`
- **Required scopes:** `list`: `view_documents`; `get`: `view_documents`; `create`: `add_documents`; `update`: `edit_documents`

**Parameters**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `action` | string | yes | Operation to perform. There is no `delete` action. |
| `comment` | string \| null | no | A revision comment. For `create`/`update`. |
| `content_base64` | string \| null | no | Raw file bytes, base64-encoded. For `create`; exactly one of |
| `custom_fields` | array<object> | no | Custom field values to set, by id. For `create`/`update`. |
| `description` | string \| null | no | Free-text description. For `create`/`update`. |
| `document_id` | integer \| null | no | The document to act on. Required for `get` and `update`. |
| `file_path` | string \| null | no | Absolute path to a file already on this server: inside |
| `folder_id` | integer \| null | no | For `list`, restrict to one folder (omit for the whole project). For |
| `limit` | integer \| null | no | For `list`, max results per call, clamped to 1-100. Default 100. |
| `name` | string \| null | no | The stored filename (DMSF's own `name` field, trap 2). For `create`, |
| `offset` | integer \| null | no | For `list`, pagination offset. Default 0. |
| `project_id` | integer \| string \| null | no | The project to act on: numeric id or slug identifier. Required for |
| `source_url` | string \| null | no | Not supported by this server. Present only so a caller who sends it |
| `title` | string \| null | no | Display title. For `create`/`update`; on `update`, defaults to the |
| `version` | string \| null | no | `"X"`, `"X.Y"`, or `"X.Y.Z"`, each part a non-negative integer. For |

**Output:** object: `document`, `document_id`, `documents`, `note`, `pagination`, `success`, `updated_fields`


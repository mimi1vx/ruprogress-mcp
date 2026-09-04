//! Per-tool OAuth scope enforcement: the map from tool name to the Redmine
//! permission(s) required to call it, and the pure resolution functions
//! `server.rs`'s hand-written `list_tools`/`call_tool` use.
//!
//! Derived from **this** server's own `redmine-client` call sites — every
//! entry below was verified
//! against the endpoint it maps to. No I/O, no async: a pure policy module
//! over `rmcp::model::JsonObject` arguments and a token's held scope set.

use std::collections::BTreeSet;

use rmcp::model::{CallToolResult, JsonObject};

use crate::tools::output::{ErrorCode, err};

/// Redmine grants this scope every operation; a token holding it bypasses
/// the map entirely, for both visibility and calls (S2). Never itself a
/// `TOOL_SCOPES` requirement.
pub const ADMIN_SCOPE: &str = "admin";

/// How a tool's scope requirement is expressed (S1).
#[derive(Debug, Clone, Copy)]
pub enum ScopeRule {
    /// The same requirement regardless of arguments. An empty slice means
    /// any authenticated token may call the tool (S2).
    Fixed(&'static [&'static str]),
    /// The requirement depends on the `action` string argument: `(action,
    /// required scopes)` pairs. An action absent from this list — including
    /// because the argument itself is missing or non-string (S4) — resolves
    /// to [`Requirement::Unchecked`], not a denial: the tool's own
    /// `action` enum will reject it with a precise message a moment later.
    PerAction(&'static [(&'static str, &'static [&'static str])]),
    /// The token must hold **at least one** of the listed scopes, not all
    /// (T7) — for a Redmine/plugin permission check that is itself an OR of
    /// two grants. Not used as a static `TOOL_SCOPES` entry today:
    /// `create_redmine_issue`/`update_redmine_issue`'s `tag_list` parameter
    /// needs this combined with an unconditional base requirement, which
    /// [`Requirement::ScopesWithAnyOf`] expresses instead — both tools
    /// resolve it by hand, argument-sensitively, alongside their other
    /// special-cased logic.
    AnyOf(&'static [&'static str]),
}

/// `update_redmine_issue`'s three possible requirements (S5). Kept as
/// `'static` slices, not computed, so [`required_for_call`] can return them
/// without allocating.
const UPDATE_ISSUE_NOTES_ONLY: &[&str] = &["add_issue_notes"];
const UPDATE_ISSUE_BASE: &[&str] = &["edit_issues"];
const UPDATE_ISSUE_WITH_SUBTASKS: &[&str] = &["edit_issues", "manage_subtasks"];
/// `story_points`/`agile_sprint_id`/`agile_position` (`RedmineUP` Agile
/// plugin) ride on `update_redmine_issue`'s existing parameters: the write
/// still goes through `PUT /issues/{id}.json`, but the mandatory
/// read-before-write hits the agile endpoint, so `view_agile_queries` is
/// required in addition.
const UPDATE_ISSUE_BASE_WITH_AGILE: &[&str] = &["edit_issues", "view_agile_queries"];
const UPDATE_ISSUE_WITH_SUBTASKS_AND_AGILE: &[&str] =
    &["edit_issues", "manage_subtasks", "view_agile_queries"];

/// The two `update_redmine_issue` fields the S5 notes-only carve-out
/// tolerates alongside `issue_id`.
const NOTES_ONLY_FIELDS: &[&str] = &["notes", "private_notes"];

/// The three agile parameter names on `update_redmine_issue`.
const AGILE_UPDATE_FIELDS: &[&str] = &["story_points", "agile_sprint_id", "agile_position"];

/// `create_redmine_issue`'s unconditional base requirement.
const CREATE_ISSUE_BASE: &[&str] = &["add_issues"];

/// The `AlphaNodes` `additional_tags` plugin's own gate on `tag_list` (T7):
/// `create_issue_tags` may mint new tag names, `edit_issue_tags` may only
/// apply existing ones — either is sufficient. Combined with each tool's
/// unconditional base requirement via [`Requirement::ScopesWithAnyOf`],
/// since a single [`ScopeRule`] cannot express "all of X, and at least one
/// of Y".
const TAG_LIST_SCOPES: &[&str] = &["create_issue_tags", "edit_issue_tags"];

/// The per-tool scope map (S1). A tool absent here is denied by
/// [`required_for_call`] (S3) — see `NOT_YET_IMPLEMENTED` below for the
/// tools deliberately left out because they are not registered yet.
///
/// `update_redmine_issue`'s entry is its common-case requirement only:
/// [`required_for_call`] and [`visible_for`] special-case its notes-only
/// carve-out and `parent_issue_id` reparent case (S5) before ever
/// consulting this table for that tool. `create_redmine_issue`'s entry is
/// likewise its common case only: [`required_for_call`] special-cases its
/// `tag_list` parameter (T7) before consulting this table.
pub static TOOL_SCOPES: &[(&str, ScopeRule)] = &[
    ("get_current_user", ScopeRule::Fixed(&[])),
    ("get_mcp_server_info", ScopeRule::Fixed(&[])),
    ("list_redmine_projects", ScopeRule::Fixed(&["view_project"])),
    ("list_redmine_trackers", ScopeRule::Fixed(&[])),
    ("list_project_trackers", ScopeRule::Fixed(&["view_project"])),
    ("list_redmine_issue_statuses", ScopeRule::Fixed(&[])),
    ("list_redmine_issue_priorities", ScopeRule::Fixed(&[])),
    ("list_redmine_users", ScopeRule::Fixed(&[])),
    ("list_redmine_queries", ScopeRule::Fixed(&["view_issues"])),
    ("list_project_issue_custom_fields", ScopeRule::Fixed(&[])),
    (
        "summarize_project_status",
        ScopeRule::Fixed(&["view_project", "view_issues"]),
    ),
    ("list_redmine_versions", ScopeRule::Fixed(&["view_issues"])),
    (
        "manage_redmine_version",
        ScopeRule::Fixed(&["manage_versions"]),
    ),
    ("list_project_members", ScopeRule::Fixed(&["view_members"])),
    ("list_redmine_roles", ScopeRule::Fixed(&[])),
    ("get_project_modules", ScopeRule::Fixed(&["view_project"])),
    (
        "manage_project_member",
        ScopeRule::Fixed(&["manage_members"]),
    ),
    ("get_redmine_issue", ScopeRule::Fixed(&["view_issues"])),
    ("list_redmine_issues", ScopeRule::Fixed(&["view_issues"])),
    (
        "search_redmine_issues",
        ScopeRule::Fixed(&["search_project", "view_issues"]),
    ),
    ("list_subtasks", ScopeRule::Fixed(&["view_issues"])),
    (
        "get_private_notes",
        ScopeRule::Fixed(&["view_issues", "view_private_notes"]),
    ),
    (
        "list_time_entries",
        ScopeRule::Fixed(&["view_time_entries"]),
    ),
    (
        "manage_time_entry",
        ScopeRule::PerAction(&[
            ("create", &["log_time"]),
            ("update", &["edit_time_entries"]),
        ]),
    ),
    ("list_time_entry_activities", ScopeRule::Fixed(&[])),
    ("import_time_entries", ScopeRule::Fixed(&["log_time"])),
    ("create_redmine_issue", ScopeRule::Fixed(CREATE_ISSUE_BASE)),
    ("update_redmine_issue", ScopeRule::Fixed(UPDATE_ISSUE_BASE)),
    ("delete_redmine_issue", ScopeRule::Fixed(&["delete_issues"])),
    (
        "copy_issue",
        ScopeRule::Fixed(&["view_issues", "add_issues"]),
    ),
    (
        "manage_issue_relation",
        ScopeRule::PerAction(&[
            ("list", &["view_issues"]),
            ("create", &["manage_issue_relations"]),
            ("delete", &["manage_issue_relations"]),
        ]),
    ),
    (
        "manage_issue_watcher",
        ScopeRule::PerAction(&[
            ("add", &["add_issue_watchers"]),
            ("remove", &["delete_issue_watchers"]),
        ]),
    ),
    (
        "manage_issue_note",
        ScopeRule::PerAction(&[
            ("edit", &["edit_issue_notes"]),
            ("set_private", &["set_notes_private"]),
        ]),
    ),
    (
        "manage_issue_category",
        ScopeRule::PerAction(&[
            ("list", &["view_issues"]),
            ("create", &["manage_categories"]),
            ("update", &["manage_categories"]),
            ("delete", &["manage_categories"]),
        ]),
    ),
    (
        "search_entire_redmine",
        ScopeRule::Fixed(&["search_project"]),
    ),
    (
        "manage_redmine_wiki_page",
        ScopeRule::PerAction(&[
            ("list", &["view_wiki_pages"]),
            ("get", &["view_wiki_pages"]),
            ("create", &["edit_wiki_pages"]),
            ("update", &["edit_wiki_pages"]),
            ("delete", &["delete_wiki_pages"]),
            ("rename", &["rename_wiki_pages"]),
        ]),
    ),
    ("get_gantt_chart", ScopeRule::Fixed(&["view_issues"])),
    ("get_redmine_attachment", ScopeRule::Fixed(&["view_files"])),
    ("list_files", ScopeRule::Fixed(&["view_files"])),
    ("delete_file", ScopeRule::Fixed(&["manage_files"])),
    ("upload_file", ScopeRule::Fixed(&["manage_files"])),
    ("cleanup_attachment_files", ScopeRule::Fixed(&[])),
    // RedmineUP Checklists plugin: no scope advertised. The plugin's own
    // Doorkeeper scope names are vendor-specific, undocumented, and vary by
    // plugin version — advertising a guess breaks the OAuth consent screen
    // outright, which is worse than deferring the authorization decision to
    // Redmine's own in-band 403.
    ("get_checklist", ScopeRule::Fixed(&[])),
    ("create_checklist_item", ScopeRule::Fixed(&[])),
    ("update_checklist_item", ScopeRule::Fixed(&[])),
    // RedmineUP Products/CRM plugins: same reasoning as Checklists above —
    // no scope advertised for any action, Redmine's own in-band 403 is the
    // authorization decision.
    ("manage_product", ScopeRule::Fixed(&[])),
    ("manage_contact", ScopeRule::Fixed(&[])),
    // DMSF plugin: unlike the three families above, its scopes were already
    // committed in the OAuth discovery documents before this tool existed
    // (`oauth/scopes.rs`'s `view_documents`/`add_documents`/
    // `edit_documents`, unconditionally advertised, not gated behind
    // REDMINE_DMSF_ENABLED) — the one family P6 does not apply to.
    (
        "manage_document",
        ScopeRule::PerAction(&[
            ("list", &["view_documents"]),
            ("get", &["view_documents"]),
            ("create", &["add_documents"]),
            ("update", &["edit_documents"]),
        ]),
    ),
];

/// Tools named in `docs/tool-contract.md`/`EXPECTED_TOOLS` with no
/// `TOOL_SCOPES` entry, because they are not registered routes yet (the
/// MCP-Apps families). Deliberately empty: the map is ported alongside the
/// tool that needs it, never ahead of it.
pub const NOT_YET_IMPLEMENTED: &[&str] = &[];

/// The outcome of resolving a tool call's scope requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    /// No authorization decision was made here; the tool's own argument
    /// validation runs next. Used for a `PerAction` tool whose `action`
    /// argument is absent, non-string, or not one of the known values (S4).
    Unchecked,
    /// The scopes the token must hold, in full, to proceed. Empty means any
    /// authenticated token.
    Scopes(&'static [&'static str]),
    /// At least one of the listed scopes must be held (T7).
    AnyOf(&'static [&'static str]),
    /// Every scope in `all` must be held, **and** at least one of `any`
    /// (T7) — `create_redmine_issue`/`update_redmine_issue`'s `tag_list`
    /// parameter combines an unconditional base requirement with the
    /// tag plugin's own create-or-edit gate.
    ScopesWithAnyOf {
        all: &'static [&'static str],
        any: &'static [&'static str],
    },
    /// No `TOOL_SCOPES` entry exists for this tool name: deny by default
    /// (S3).
    Unmapped,
}

fn action_str(args: Option<&JsonObject>) -> Option<&str> {
    args?.get("action")?.as_str()
}

/// S5's notes-only carve-out: every argument key besides `issue_id` is
/// `notes`/`private_notes`, and there is at least one such key (an
/// argument-less call is rejected by the tool's own validation, not by
/// scope resolution).
fn is_notes_only_update(args: &JsonObject) -> bool {
    let mut saw_notes_field = false;
    for key in args.keys() {
        if key == "issue_id" {
            continue;
        }
        if !NOTES_ONLY_FIELDS.contains(&key.as_str()) {
            return false;
        }
        saw_notes_field = true;
    }
    saw_notes_field
}

/// `true` if any of `story_points`/`agile_sprint_id`/`agile_position` is
/// present.
fn has_agile_field(args: &JsonObject) -> bool {
    AGILE_UPDATE_FIELDS.iter().any(|f| args.contains_key(*f))
}

/// `update_redmine_issue`'s requirement (S5, T7): the notes-only carve-out,
/// otherwise the `edit_issues` base case plus `manage_subtasks` when
/// `parent_issue_id` is present (whether set directly or unset via
/// `clear_fields`; reparenting is its own Redmine permission)
/// and/or `view_agile_queries` when an agile field is present (its
/// mandatory read hits the agile endpoint even though the write shares
/// `update_redmine_issue`'s own `PUT`), and/or the [`TAG_LIST_SCOPES`]
/// any-of when `tag_list` is present. `tag_list` never joins the notes-only
/// carve-out — it is not a `NOTES_ONLY_FIELDS` entry, so a call combining it
/// with only `notes`/`private_notes` still requires `edit_issues`.
fn update_redmine_issue_requirement(args: Option<&JsonObject>) -> Requirement {
    let Some(args) = args else {
        return Requirement::Scopes(UPDATE_ISSUE_BASE);
    };
    if is_notes_only_update(args) {
        return Requirement::Scopes(UPDATE_ISSUE_NOTES_ONLY);
    }
    let agile = has_agile_field(args);
    let reparenting = args.contains_key("parent_issue_id")
        || args
            .get("clear_fields")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("parent_issue_id")));
    let base: &'static [&'static str] = if reparenting {
        if agile {
            UPDATE_ISSUE_WITH_SUBTASKS_AND_AGILE
        } else {
            UPDATE_ISSUE_WITH_SUBTASKS
        }
    } else if agile {
        UPDATE_ISSUE_BASE_WITH_AGILE
    } else {
        UPDATE_ISSUE_BASE
    };
    if args.contains_key("tag_list") {
        Requirement::ScopesWithAnyOf {
            all: base,
            any: TAG_LIST_SCOPES,
        }
    } else {
        Requirement::Scopes(base)
    }
}

/// `custom_fields` (7f1, F22) needs no scope rule of its own on either
/// tool: writing a custom field value is covered by the tool's existing
/// `add_issues`/`edit_issues` base, and the `name`-resolution lookup needs
/// only project-view, which every path able to call these tools already
/// holds. Deliberately not a table entry.
///
/// `create_redmine_issue`'s requirement (T7): the unconditional `add_issues`
/// base, plus the [`TAG_LIST_SCOPES`] any-of when `tag_list` is present.
fn create_redmine_issue_requirement(args: Option<&JsonObject>) -> Requirement {
    let has_tag_list = args.is_some_and(|a| a.contains_key("tag_list"));
    if has_tag_list {
        Requirement::ScopesWithAnyOf {
            all: CREATE_ISSUE_BASE,
            any: TAG_LIST_SCOPES,
        }
    } else {
        Requirement::Scopes(CREATE_ISSUE_BASE)
    }
}

/// Resolve `tool`'s scope requirement for this call's `args` (S1–S5, T7).
#[must_use]
pub fn required_for_call(tool: &str, args: Option<&JsonObject>) -> Requirement {
    if tool == "update_redmine_issue" {
        return update_redmine_issue_requirement(args);
    }
    if tool == "create_redmine_issue" {
        return create_redmine_issue_requirement(args);
    }
    let Some((_, rule)) = TOOL_SCOPES.iter().find(|(name, _)| *name == tool) else {
        return Requirement::Unmapped;
    };
    match rule {
        ScopeRule::Fixed(scopes) => Requirement::Scopes(scopes),
        ScopeRule::AnyOf(scopes) => Requirement::AnyOf(scopes),
        ScopeRule::PerAction(actions) => {
            let Some(action) = action_str(args) else {
                return Requirement::Unchecked;
            };
            actions
                .iter()
                .find(|(a, _)| *a == action)
                .map_or(Requirement::Unchecked, |(_, scopes)| {
                    Requirement::Scopes(scopes)
                })
        }
    }
}

/// `true` if `held` carries the [`ADMIN_SCOPE`] bypass (S2).
#[must_use]
pub fn is_admin(held: &BTreeSet<String>) -> bool {
    held.contains(ADMIN_SCOPE)
}

/// The entries of `required` that `held` does not contain, in `required`'s
/// order. Does **not** apply the admin bypass — callers check
/// [`is_admin`] first.
#[must_use]
pub fn missing(required: &[&'static str], held: &BTreeSet<String>) -> Vec<&'static str> {
    required
        .iter()
        .copied()
        .filter(|scope| !held.contains(*scope))
        .collect()
}

/// `true` if `held` contains at least one of `candidates` (the any-of half
/// of [`Requirement::AnyOf`]/[`Requirement::ScopesWithAnyOf`]).
#[must_use]
pub fn any_held(candidates: &[&'static str], held: &BTreeSet<String>) -> bool {
    candidates.iter().any(|scope| held.contains(*scope))
}

/// Whether `tool` should appear in `tools/list` for a token holding `held`
/// (S2, S5). A `Fixed` tool is visible iff every required scope is held; an
/// `AnyOf` tool is visible iff at least one listed scope is held; a
/// `PerAction` tool is visible iff at least one action is fully reachable —
/// hiding it entirely just because one action needs more scope would hide
/// the actions the token *can* already use.
///
/// `tag_list`'s `ScopesWithAnyOf` requirement (T7) is deliberately **not**
/// reflected here: visibility cannot know a future call's arguments, and
/// `create_redmine_issue`/`update_redmine_issue` already default-visible via
/// their unconditional base requirement.
#[must_use]
pub fn visible_for(tool: &str, held: &BTreeSet<String>) -> bool {
    if is_admin(held) {
        return true;
    }
    if tool == "update_redmine_issue" {
        return held.contains("edit_issues") || held.contains("add_issue_notes");
    }
    let Some((_, rule)) = TOOL_SCOPES.iter().find(|(name, _)| *name == tool) else {
        return false;
    };
    match rule {
        ScopeRule::Fixed(scopes) => scopes.iter().all(|scope| held.contains(*scope)),
        ScopeRule::AnyOf(scopes) => any_held(scopes, held),
        ScopeRule::PerAction(actions) => actions
            .iter()
            .any(|(_, scopes)| scopes.iter().all(|scope| held.contains(*scope))),
    }
}

/// The S6 in-band denial envelope: `{error, code: "INSUFFICIENT_SCOPE",
/// retryable: false, hint}`, naming the missing scope(s). An empty `missing`
/// means `tool` has no `TOOL_SCOPES` entry at all (S3) — the message says so
/// instead of claiming a specific (nonexistent) requirement.
pub(crate) fn insufficient_scope_result(tool: &str, missing: &[&str]) -> CallToolResult {
    let message = if missing.is_empty() {
        format!(
            "{tool} has no configured OAuth scope requirement in this deployment and is denied \
             by default"
        )
    } else {
        format!(
            "this token is missing the scope(s) required to call {tool}: {}",
            missing.join(", ")
        )
    };
    err(
        ErrorCode::InsufficientScope,
        message,
        Some("re-authorize with the missing scope(s), or use a different tool"),
    )
}

/// The S6 in-band denial for a [`Requirement::AnyOf`]/
/// [`Requirement::ScopesWithAnyOf`] failure: the token holds none of an
/// "at least one of" requirement.
pub(crate) fn insufficient_any_of_result(tool: &str, any: &[&str]) -> CallToolResult {
    let message = format!(
        "this token is missing every scope that would satisfy {tool}'s requirement: at least \
         one of {}",
        any.join(", ")
    );
    err(
        ErrorCode::InsufficientScope,
        message,
        Some("re-authorize with one of the listed scopes, or use a different tool"),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn scopes(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(ToString::to_string).collect()
    }

    fn args(pairs: &[(&str, serde_json::Value)]) -> JsonObject {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn fixed_rule_match_and_mismatch() {
        let held = scopes(&["view_project"]);
        assert!(visible_for("list_redmine_projects", &held));
        assert!(!visible_for("list_redmine_versions", &held));
    }

    #[test]
    fn per_action_match_and_mismatch() {
        let req = required_for_call(
            "manage_time_entry",
            Some(&args(&[("action", "create".into())])),
        );
        assert_eq!(req, Requirement::Scopes(&["log_time"]));
        let req = required_for_call(
            "manage_time_entry",
            Some(&args(&[("action", "update".into())])),
        );
        assert_eq!(req, Requirement::Scopes(&["edit_time_entries"]));
    }

    #[test]
    fn per_action_absent_action_passes_through_unchecked() {
        let req = required_for_call("manage_time_entry", Some(&args(&[])));
        assert_eq!(req, Requirement::Unchecked);
        let req = required_for_call("manage_time_entry", None);
        assert_eq!(req, Requirement::Unchecked);
    }

    #[test]
    fn per_action_non_string_action_passes_through_unchecked() {
        let req = required_for_call(
            "manage_time_entry",
            Some(&args(&[("action", serde_json::json!(1))])),
        );
        assert_eq!(req, Requirement::Unchecked);
    }

    #[test]
    fn per_action_unknown_action_value_passes_through_unchecked() {
        let req = required_for_call(
            "manage_time_entry",
            Some(&args(&[("action", "bogus".into())])),
        );
        assert_eq!(req, Requirement::Unchecked);
    }

    #[test]
    fn notes_only_update_requires_add_issue_notes() {
        let req = required_for_call(
            "update_redmine_issue",
            Some(&args(&[
                ("issue_id", serde_json::json!(1)),
                ("notes", "a comment".into()),
            ])),
        );
        assert_eq!(req, Requirement::Scopes(UPDATE_ISSUE_NOTES_ONLY));
    }

    #[test]
    fn notes_and_private_notes_only_still_counts_as_notes_only() {
        let req = required_for_call(
            "update_redmine_issue",
            Some(&args(&[
                ("issue_id", serde_json::json!(1)),
                ("notes", "a comment".into()),
                ("private_notes", true.into()),
            ])),
        );
        assert_eq!(req, Requirement::Scopes(UPDATE_ISSUE_NOTES_ONLY));
    }

    #[test]
    fn notes_plus_another_field_requires_edit_issues() {
        let req = required_for_call(
            "update_redmine_issue",
            Some(&args(&[
                ("issue_id", serde_json::json!(1)),
                ("notes", "a comment".into()),
                ("subject", "new subject".into()),
            ])),
        );
        assert_eq!(req, Requirement::Scopes(UPDATE_ISSUE_BASE));
    }

    #[test]
    fn notes_plus_uploads_requires_edit_issues() {
        let req = required_for_call(
            "update_redmine_issue",
            Some(&args(&[
                ("issue_id", serde_json::json!(1)),
                ("notes", "a comment".into()),
                ("uploads", serde_json::json!([])),
            ])),
        );
        assert_eq!(req, Requirement::Scopes(UPDATE_ISSUE_BASE));
    }

    #[test]
    fn reparenting_requires_manage_subtasks_in_addition_to_edit_issues() {
        let req = required_for_call(
            "update_redmine_issue",
            Some(&args(&[
                ("issue_id", serde_json::json!(1)),
                ("parent_issue_id", serde_json::json!(2)),
            ])),
        );
        assert_eq!(req, Requirement::Scopes(UPDATE_ISSUE_WITH_SUBTASKS));
    }

    #[test]
    fn unparenting_via_clear_fields_requires_manage_subtasks_in_addition_to_edit_issues() {
        let req = required_for_call(
            "update_redmine_issue",
            Some(&args(&[
                ("issue_id", serde_json::json!(1)),
                ("clear_fields", serde_json::json!(["parent_issue_id"])),
            ])),
        );
        assert_eq!(req, Requirement::Scopes(UPDATE_ISSUE_WITH_SUBTASKS));
    }

    #[test]
    fn an_agile_field_requires_view_agile_queries_in_addition_to_edit_issues() {
        let req = required_for_call(
            "update_redmine_issue",
            Some(&args(&[
                ("issue_id", serde_json::json!(1)),
                ("story_points", serde_json::json!(8)),
            ])),
        );
        assert_eq!(req, Requirement::Scopes(UPDATE_ISSUE_BASE_WITH_AGILE));
    }

    #[test]
    fn an_agile_field_plus_reparenting_requires_all_three_scopes() {
        let req = required_for_call(
            "update_redmine_issue",
            Some(&args(&[
                ("issue_id", serde_json::json!(1)),
                ("parent_issue_id", serde_json::json!(2)),
                ("agile_sprint_id", serde_json::json!(7)),
            ])),
        );
        assert_eq!(
            req,
            Requirement::Scopes(UPDATE_ISSUE_WITH_SUBTASKS_AND_AGILE)
        );
    }

    #[test]
    fn a_non_agile_field_does_not_require_view_agile_queries() {
        let req = required_for_call(
            "update_redmine_issue",
            Some(&args(&[
                ("issue_id", serde_json::json!(1)),
                ("subject", "new subject".into()),
            ])),
        );
        assert_eq!(req, Requirement::Scopes(UPDATE_ISSUE_BASE));
    }

    // --- T7: tag_list's any-of requirement ---

    #[test]
    fn update_issue_tag_list_requires_edit_issues_and_either_tag_scope() {
        let req = required_for_call(
            "update_redmine_issue",
            Some(&args(&[
                ("issue_id", serde_json::json!(1)),
                ("tag_list", serde_json::json!(["a"])),
            ])),
        );
        assert_eq!(
            req,
            Requirement::ScopesWithAnyOf {
                all: UPDATE_ISSUE_BASE,
                any: TAG_LIST_SCOPES,
            }
        );
    }

    #[test]
    fn update_issue_tag_list_passes_with_create_issue_tags_alone() {
        let held = scopes(&["edit_issues", "create_issue_tags"]);
        match required_for_call(
            "update_redmine_issue",
            Some(&args(&[
                ("issue_id", serde_json::json!(1)),
                ("tag_list", serde_json::json!(["a"])),
            ])),
        ) {
            Requirement::ScopesWithAnyOf { all, any } => {
                assert!(missing(all, &held).is_empty());
                assert!(any_held(any, &held));
            }
            other => panic!("unexpected requirement: {other:?}"),
        }
    }

    #[test]
    fn update_issue_tag_list_passes_with_edit_issue_tags_alone() {
        let held = scopes(&["edit_issues", "edit_issue_tags"]);
        match required_for_call(
            "update_redmine_issue",
            Some(&args(&[
                ("issue_id", serde_json::json!(1)),
                ("tag_list", serde_json::json!(["a"])),
            ])),
        ) {
            Requirement::ScopesWithAnyOf { all, any } => {
                assert!(missing(all, &held).is_empty());
                assert!(any_held(any, &held));
            }
            other => panic!("unexpected requirement: {other:?}"),
        }
    }

    #[test]
    fn update_issue_tag_list_denies_with_neither_tag_scope() {
        let held = scopes(&["edit_issues"]);
        match required_for_call(
            "update_redmine_issue",
            Some(&args(&[
                ("issue_id", serde_json::json!(1)),
                ("tag_list", serde_json::json!(["a"])),
            ])),
        ) {
            Requirement::ScopesWithAnyOf { any, .. } => assert!(!any_held(any, &held)),
            other => panic!("unexpected requirement: {other:?}"),
        }
    }

    #[test]
    fn update_issue_notes_plus_tag_list_is_not_the_notes_only_carve_out() {
        let req = required_for_call(
            "update_redmine_issue",
            Some(&args(&[
                ("issue_id", serde_json::json!(1)),
                ("notes", "a comment".into()),
                ("tag_list", serde_json::json!(["a"])),
            ])),
        );
        assert_eq!(
            req,
            Requirement::ScopesWithAnyOf {
                all: UPDATE_ISSUE_BASE,
                any: TAG_LIST_SCOPES,
            }
        );
    }

    #[test]
    fn create_issue_without_tag_list_is_unaffected() {
        let req = required_for_call(
            "create_redmine_issue",
            Some(&args(&[("subject", "x".into())])),
        );
        assert_eq!(req, Requirement::Scopes(CREATE_ISSUE_BASE));
    }

    #[test]
    fn create_issue_tag_list_requires_add_issues_and_either_tag_scope() {
        let req = required_for_call(
            "create_redmine_issue",
            Some(&args(&[("tag_list", serde_json::json!(["a"]))])),
        );
        assert_eq!(
            req,
            Requirement::ScopesWithAnyOf {
                all: CREATE_ISSUE_BASE,
                any: TAG_LIST_SCOPES,
            }
        );
    }

    #[test]
    fn create_issue_tag_list_denies_add_issues_alone_with_neither_tag_scope() {
        let held = scopes(&["add_issues"]);
        match required_for_call(
            "create_redmine_issue",
            Some(&args(&[("tag_list", serde_json::json!(["a"]))])),
        ) {
            Requirement::ScopesWithAnyOf { all, any } => {
                assert!(missing(all, &held).is_empty());
                assert!(!any_held(any, &held));
            }
            other => panic!("unexpected requirement: {other:?}"),
        }
    }

    #[test]
    fn any_of_scope_rule_passes_with_either_alone_and_denies_with_neither() {
        let candidates: &[&str] = &["a", "b"];
        assert!(any_held(candidates, &scopes(&["a"])));
        assert!(any_held(candidates, &scopes(&["b"])));
        assert!(!any_held(candidates, &scopes(&["c"])));
    }

    #[test]
    fn update_issue_visibility_ors_edit_and_notes_scopes() {
        assert!(visible_for(
            "update_redmine_issue",
            &scopes(&["edit_issues"])
        ));
        assert!(visible_for(
            "update_redmine_issue",
            &scopes(&["add_issue_notes"])
        ));
        assert!(!visible_for(
            "update_redmine_issue",
            &scopes(&["view_issues"])
        ));
    }

    #[test]
    fn admin_bypasses_visibility_for_every_tool_including_unmapped() {
        let held = scopes(&[ADMIN_SCOPE]);
        assert!(visible_for("list_redmine_projects", &held));
        assert!(visible_for("update_redmine_issue", &held));
        assert!(visible_for("not_a_real_tool", &held));
    }

    #[test]
    fn unmapped_tool_is_denied_and_hidden() {
        assert_eq!(
            required_for_call("not_a_real_tool", None),
            Requirement::Unmapped
        );
        assert!(!visible_for(
            "not_a_real_tool",
            &scopes(&["admin_adjacent"])
        ));
    }

    #[test]
    fn admin_is_never_a_required_scope() {
        for (_, rule) in TOOL_SCOPES {
            match rule {
                ScopeRule::Fixed(scopes) | ScopeRule::AnyOf(scopes) => {
                    assert!(!scopes.contains(&ADMIN_SCOPE));
                }
                ScopeRule::PerAction(actions) => {
                    for (_, scopes) in *actions {
                        assert!(!scopes.contains(&ADMIN_SCOPE));
                    }
                }
            }
        }
    }

    #[test]
    fn missing_lists_only_unheld_scopes_in_order() {
        let held = scopes(&["view_issues"]);
        assert_eq!(
            missing(&["view_project", "view_issues"], &held),
            vec!["view_project"]
        );
        assert!(missing(&["view_issues"], &held).is_empty());
    }
}

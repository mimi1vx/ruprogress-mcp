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
}

/// `update_redmine_issue`'s three possible requirements (S5). Kept as
/// `'static` slices, not computed, so [`required_for_call`] can return them
/// without allocating.
const UPDATE_ISSUE_NOTES_ONLY: &[&str] = &["add_issue_notes"];
const UPDATE_ISSUE_BASE: &[&str] = &["edit_issues"];
const UPDATE_ISSUE_WITH_SUBTASKS: &[&str] = &["edit_issues", "manage_subtasks"];

/// The two `update_redmine_issue` fields the S5 notes-only carve-out
/// tolerates alongside `issue_id`.
const NOTES_ONLY_FIELDS: &[&str] = &["notes", "private_notes"];

/// The per-tool scope map (S1). A tool absent here is denied by
/// [`required_for_call`] (S3) — see `NOT_YET_IMPLEMENTED` below for the
/// tools deliberately left out because they are not registered yet.
///
/// `update_redmine_issue`'s entry is its common-case requirement only:
/// [`required_for_call`] and [`visible_for`] special-case its notes-only
/// carve-out and `parent_issue_id` reparent case (S5) before ever
/// consulting this table for that tool.
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
    ("create_redmine_issue", ScopeRule::Fixed(&["add_issues"])),
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
];

/// Tools named in `docs/tool-contract.md`/`EXPECTED_TOOLS` with no
/// `TOOL_SCOPES` entry, because they are not registered routes yet
/// (`manage_document`, `manage_product`, `manage_contact`, and the MCP-Apps
/// families). Deliberately empty: the map is ported alongside the tool that
/// needs it, never ahead of it.
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

/// `update_redmine_issue`'s requirement (S5): the notes-only carve-out,
/// otherwise the `edit_issues` base case plus `manage_subtasks` when
/// `parent_issue_id` is present (reparenting is its own Redmine
/// permission).
fn update_redmine_issue_requirement(args: Option<&JsonObject>) -> &'static [&'static str] {
    let Some(args) = args else {
        return UPDATE_ISSUE_BASE;
    };
    if is_notes_only_update(args) {
        return UPDATE_ISSUE_NOTES_ONLY;
    }
    if args.contains_key("parent_issue_id") {
        return UPDATE_ISSUE_WITH_SUBTASKS;
    }
    UPDATE_ISSUE_BASE
}

/// Resolve `tool`'s scope requirement for this call's `args` (S1–S5).
#[must_use]
pub fn required_for_call(tool: &str, args: Option<&JsonObject>) -> Requirement {
    if tool == "update_redmine_issue" {
        return Requirement::Scopes(update_redmine_issue_requirement(args));
    }
    let Some((_, rule)) = TOOL_SCOPES.iter().find(|(name, _)| *name == tool) else {
        return Requirement::Unmapped;
    };
    match rule {
        ScopeRule::Fixed(scopes) => Requirement::Scopes(scopes),
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

/// Whether `tool` should appear in `tools/list` for a token holding `held`
/// (S2, S5). A `Fixed` tool is visible iff every required scope is held; a
/// `PerAction` tool is visible iff at least one action is fully reachable —
/// hiding it entirely just because one action needs more scope would hide
/// the actions the token *can* already use.
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
                ScopeRule::Fixed(scopes) => assert!(!scopes.contains(&ADMIN_SCOPE)),
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
